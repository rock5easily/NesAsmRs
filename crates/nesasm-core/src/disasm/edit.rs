//! Edits of the annotation file: operations that add, replace or remove
//! entries by key, the suggestions made from issues in the same form, and
//! the canonical text annotations are written in.

use super::analysis::{Analysis, Fix, Loc, Memory};
use super::config::{
    Address, BankLayout, CommentDef, CommentPosition, DisasmConfig, DisasmOptions, EntryPoint,
    LabelDef, OperandDef, PatchDef, Provenance, ReferenceDef, RegionDef, SymbolDef,
};
#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// What an operation does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    /// Add `item`, replacing the entries with the same key.
    Upsert,
    /// Remove every entry that `key` names.
    Remove,
    /// Change fields of `options` (`item`); a field set to null returns to
    /// its default.
    Set,
}

/// The list of the annotations an operation changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AnnotationKind {
    EntryPoints,
    Labels,
    Symbols,
    Regions,
    Comments,
    Operands,
    References,
    Banks,
    Patches,
    Options,
}

impl AnnotationKind {
    fn name(self) -> &'static str {
        match self {
            Self::EntryPoints => "entry_points",
            Self::Labels => "labels",
            Self::Symbols => "symbols",
            Self::Regions => "regions",
            Self::Comments => "comments",
            Self::Operands => "operands",
            Self::References => "references",
            Self::Banks => "banks",
            Self::Patches => "patches",
            Self::Options => "options",
        }
    }
}

/// The entries a `remove` operation names: by `bank` and `address` (the
/// start of regions and patches; also `position` for comments, both
/// positions when it is left out), by `name` (labels, symbols) or by `bank`
/// (banks). `bank` may be left out when one bank holds the address.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EntryKey {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<usize>,
    #[serde(alias = "start", skip_serializing_if = "Option::is_none")]
    pub address: Option<Address>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<CommentPosition>,
}

/// A change to the annotations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub op: OperationKind,
    pub kind: AnnotationKind,
    /// `upsert`: the whole entry, with the fields of the annotation file
    /// (`{"bank": 30, "address": "$DEB1", "target_bank": 27}`). `set`: the
    /// fields of `options` to change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<Value>,
    /// `remove`: the entries to remove.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<EntryKey>,
    /// What was observed that supports the change (callers, the code that
    /// reads a table, documentation). Required for entries that override
    /// what the analysis finds (see `requires_evidence`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// What an operation replaced or removed.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Applied {
    /// Index of the operation.
    pub index: usize,
    /// The entries (or options) it replaced or removed, as they were.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous: Vec<Value>,
}

/// One way to settle an issue: operations to apply together.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Suggestion {
    pub description: String,
    pub operations: Vec<Operation>,
    /// Fields the analysis cannot determine, left null in the items (a
    /// table's `length`); fill them in before applying.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    /// The operations override what the analysis finds, so they need evidence.
    pub evidence_required: bool,
}

/// Whether an operation needs evidence: it adds an entry that overrides
/// what the analysis finds (`references`; entry points with `no_return`,
/// `inline_bytes`, `switch_banks` or `keeps_banks`; `code` regions and
/// tables with `code_targets`).
pub fn requires_evidence(operation: &Operation) -> bool {
    let Some(item) = operation
        .item
        .as_ref()
        .filter(|_| operation.op == OperationKind::Upsert)
    else {
        return false;
    };
    // A null field is still to be filled in.
    let set = |name: &str| {
        item.get(name)
            .is_some_and(|v| v != &json!(false) && v != &json!(0))
    };
    // A hypothesis needs a reason too.
    if item.get("status") == Some(&json!("hypothesis")) {
        return true;
    }
    match operation.kind {
        AnnotationKind::References => true,
        AnnotationKind::EntryPoints => {
            set("no_return") || set("inline_bytes") || set("switch_banks") || set("keeps_banks")
        }
        AnnotationKind::Regions => item.get("type") == Some(&json!("code")) || set("code_targets"),
        _ => false,
    }
}

/// What identifies an entry of a list.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    At(Option<usize>, u32),
    Name(String),
    Comment(Option<usize>, u32, CommentPosition),
    Bank(usize),
}

impl Key {
    /// Whether an entry with the key `entry` has this key, or would have it
    /// with a bank: one that leaves out a bank no single bank holds the
    /// address of can still be replaced or removed.
    fn names(&self, entry: &Key) -> bool {
        match (self, entry) {
            (Self::At(_, address), Self::At(None, other)) => address == other,
            (Self::Comment(_, address, position), Self::Comment(None, other, at)) => {
                address == other && position == at
            }
            _ => self == entry,
        }
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let at = |bank: &Option<usize>, address: u32| match bank {
            Some(bank) => format!("bank {bank} {}", Address(address)),
            None => Address(address).to_string(),
        };
        match self {
            Self::At(bank, address) => write!(f, "{}", at(bank, *address)),
            Self::Name(name) => write!(f, "'{name}'"),
            Self::Comment(bank, address, position) => {
                let position = match position {
                    CommentPosition::Line => "line",
                    CommentPosition::Above => "above",
                };
                write!(f, "{} ({position})", at(bank, *address))
            }
            Self::Bank(bank) => write!(f, "bank {bank}"),
        }
    }
}

/// An entry of one of the annotation lists.
trait Entry: Clone + Serialize + DeserializeOwned + Provenance {
    /// The bank and address in the ROM the entry names, if any.
    fn location(&mut self) -> Option<(&mut Option<usize>, Address)>;
    fn key(&self) -> Key;
    /// Where the entry goes in the written file; entries with the same place
    /// keep their order (the sort is stable).
    fn order(&self) -> (Option<usize>, u32, String);
    /// Whether the list is put in the order of `order`.
    const SORTED: bool = true;
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
}

fn at_order(bank: Option<usize>, address: Address) -> (Option<usize>, u32, String) {
    (bank, address.0, String::new())
}

impl Entry for EntryPoint {
    fn location(&mut self) -> Option<(&mut Option<usize>, Address)> {
        Some((&mut self.bank, self.address))
    }
    fn key(&self) -> Key {
        Key::At(self.bank, self.address.0)
    }
    fn order(&self) -> (Option<usize>, u32, String) {
        at_order(self.bank, self.address)
    }
}

impl Entry for LabelDef {
    fn location(&mut self) -> Option<(&mut Option<usize>, Address)> {
        Some((&mut self.bank, self.address))
    }
    fn key(&self) -> Key {
        Key::Name(self.name.clone())
    }
    fn order(&self) -> (Option<usize>, u32, String) {
        // Labels at one address keep their order: the first names the place.
        (self.bank, self.address.0, String::new())
    }
}

impl Entry for SymbolDef {
    // The first symbol whose range holds a value names it, also when the
    // ranges of symbols of other values overlap: their order is kept.
    const SORTED: bool = false;
    fn location(&mut self) -> Option<(&mut Option<usize>, Address)> {
        None
    }
    fn key(&self) -> Key {
        Key::Name(self.name.clone())
    }
    fn order(&self) -> (Option<usize>, u32, String) {
        (None, self.value.0, String::new())
    }
}

impl Entry for RegionDef {
    fn location(&mut self) -> Option<(&mut Option<usize>, Address)> {
        Some((&mut self.bank, self.start))
    }
    fn key(&self) -> Key {
        Key::At(self.bank, self.start.0)
    }
    fn order(&self) -> (Option<usize>, u32, String) {
        at_order(self.bank, self.start)
    }
    fn check(&self) -> Result<(), String> {
        check_range(self.end, self.length)
    }
}

impl Entry for CommentDef {
    fn location(&mut self) -> Option<(&mut Option<usize>, Address)> {
        Some((&mut self.bank, self.address))
    }
    fn key(&self) -> Key {
        Key::Comment(self.bank, self.address.0, self.position)
    }
    fn order(&self) -> (Option<usize>, u32, String) {
        let position = match self.position {
            CommentPosition::Line => "0",
            CommentPosition::Above => "1",
        };
        (self.bank, self.address.0, position.into())
    }
}

impl Entry for OperandDef {
    fn location(&mut self) -> Option<(&mut Option<usize>, Address)> {
        Some((&mut self.bank, self.address))
    }
    fn key(&self) -> Key {
        Key::At(self.bank, self.address.0)
    }
    fn order(&self) -> (Option<usize>, u32, String) {
        at_order(self.bank, self.address)
    }
}

impl Entry for ReferenceDef {
    fn location(&mut self) -> Option<(&mut Option<usize>, Address)> {
        Some((&mut self.bank, self.address))
    }
    fn key(&self) -> Key {
        Key::At(self.bank, self.address.0)
    }
    fn order(&self) -> (Option<usize>, u32, String) {
        at_order(self.bank, self.address)
    }
}

impl Entry for BankLayout {
    fn location(&mut self) -> Option<(&mut Option<usize>, Address)> {
        None
    }
    fn key(&self) -> Key {
        Key::Bank(self.bank)
    }
    fn order(&self) -> (Option<usize>, u32, String) {
        (Some(self.bank), 0, String::new())
    }
}

impl Entry for PatchDef {
    fn location(&mut self) -> Option<(&mut Option<usize>, Address)> {
        Some((&mut self.bank, self.start))
    }
    fn key(&self) -> Key {
        Key::At(self.bank, self.start.0)
    }
    fn order(&self) -> (Option<usize>, u32, String) {
        at_order(self.bank, self.start)
    }
    fn check(&self) -> Result<(), String> {
        check_range(self.end, self.length)
    }
}

fn check_range(end: Option<Address>, length: Option<u32>) -> Result<(), String> {
    match (end, length) {
        (None, None) => Err("give end or length".into()),
        _ => Ok(()),
    }
}

fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Gives an entry that names an address the bank holding it, when one does.
/// The error is why no single bank holds the address.
fn normalize<T: Entry>(entry: &mut T, memory: Option<&Memory>) -> Result<(), String> {
    if let Some(memory) = memory
        && let Some((bank, address)) = entry.location()
        && bank.is_none()
    {
        *bank = Some(memory.locate(None, address)?.bank);
    }
    Ok(())
}

/// Adds `item` to `list`, replacing the entries with its key.
fn upsert<T: Entry>(
    list: &mut Vec<T>,
    item: &Value,
    memory: Option<&Memory>,
    evidence: Option<&str>,
) -> Result<Vec<Value>, String> {
    let fields = item.as_object().ok_or("item must be an object")?;
    if let Some((name, _)) = fields.iter().find(|(_, v)| v.is_null()) {
        return Err(format!("fill in {name} (null)"));
    }
    let mut item: T = serde_json::from_value(item.clone()).map_err(|e| e.to_string())?;
    item.check()?;
    normalize(&mut item, memory)?;
    // The operation's evidence is added to what the item gives.
    if let Some(text) = evidence.map(str::trim).filter(|t| !t.is_empty())
        && !item.evidence().iter().any(|e| e == text)
    {
        item.evidence_mut().push(text.to_owned());
    }
    let key = item.key();
    // In the place of the first entry it replaces, as order can matter.
    let place = list.iter().position(|entry| key.names(&entry.key()));
    let mut previous = Vec::new();
    list.retain(|entry| {
        let same = key.names(&entry.key());
        if same {
            previous.push(to_value(entry));
        }
        !same
    });
    list.insert(place.unwrap_or(list.len()), item);
    Ok(previous)
}

/// Removes the entries of `list` that `key` names.
fn remove<T: Entry>(
    list: &mut Vec<T>,
    kind: AnnotationKind,
    key: &EntryKey,
    memory: Option<&Memory>,
) -> Result<Vec<Value>, String> {
    let address = || {
        key.address
            .ok_or_else(|| format!("{} are removed by address", kind.name()))
    };
    // Without a bank no single bank holds, only entries without one match.
    let bank = |address: Address| match (key.bank, memory) {
        (Some(bank), _) => Some(bank),
        (None, Some(memory)) => memory.locate(None, address).ok().map(|l| l.bank),
        (None, None) => None,
    };
    let matches: Box<dyn Fn(&Key) -> bool> = match kind {
        AnnotationKind::Labels | AnnotationKind::Symbols => {
            let name = key
                .name
                .clone()
                .ok_or_else(|| format!("{} are removed by name", kind.name()))?;
            Box::new(move |k| *k == Key::Name(name.clone()))
        }
        AnnotationKind::Banks => {
            let bank = key.bank.ok_or("banks are removed by bank")?;
            Box::new(move |k| *k == Key::Bank(bank))
        }
        AnnotationKind::Comments => {
            let address = address()?;
            let bank = bank(address);
            let position = key.position;
            Box::new(move |k| {
                matches!(k, Key::Comment(_, _, p)
                    if position.is_none_or(|q| q == *p)
                        && Key::Comment(bank, address.0, *p).names(k))
            })
        }
        _ => {
            let address = address()?;
            let bank = bank(address);
            Box::new(move |k| Key::At(bank, address.0).names(k))
        }
    };
    let mut previous = Vec::new();
    list.retain(|entry| {
        let found = matches(&entry.key());
        if found {
            previous.push(to_value(entry));
        }
        !found
    });
    if previous.is_empty() {
        return Err("no entry matches the key".into());
    }
    Ok(previous)
}

/// Reports keys that more than one entry of `list` has.
fn duplicates<T: Entry>(kind: AnnotationKind, list: &[T], errors: &mut Vec<String>) {
    let mut counts: BTreeMap<Key, usize> = BTreeMap::new();
    for entry in list {
        *counts.entry(entry.key()).or_default() += 1;
    }
    for (key, count) in counts.into_iter().filter(|(_, n)| *n > 1) {
        errors.push(format!(
            "{}: {count} entries have the key {key}; remove them and give one",
            kind.name()
        ));
    }
}

/// Puts `list` in canonical order, unless the order of its entries matters.
fn sort<T: Entry>(list: &mut [T]) {
    if T::SORTED {
        list.sort_by_key(|entry| entry.order());
    }
}

/// Runs `$body` with `$list` bound to the list of `$kind` in `$config`
/// (not `options`).
macro_rules! with_list {
    ($config:expr, $kind:expr, $list:ident => $body:expr) => {
        match $kind {
            AnnotationKind::EntryPoints => {
                let $list = &mut $config.entry_points;
                $body
            }
            AnnotationKind::Labels => {
                let $list = &mut $config.labels;
                $body
            }
            AnnotationKind::Symbols => {
                let $list = &mut $config.symbols;
                $body
            }
            AnnotationKind::Regions => {
                let $list = &mut $config.regions;
                $body
            }
            AnnotationKind::Comments => {
                let $list = &mut $config.comments;
                $body
            }
            AnnotationKind::Operands => {
                let $list = &mut $config.operands;
                $body
            }
            AnnotationKind::References => {
                let $list = &mut $config.references;
                $body
            }
            AnnotationKind::Banks => {
                let $list = &mut $config.banks;
                $body
            }
            AnnotationKind::Patches | AnnotationKind::Options => {
                let $list = &mut $config.patches;
                $body
            }
        }
    };
}

const LIST_KINDS: [AnnotationKind; 9] = [
    AnnotationKind::Banks,
    AnnotationKind::EntryPoints,
    AnnotationKind::Labels,
    AnnotationKind::Symbols,
    AnnotationKind::Regions,
    AnnotationKind::Comments,
    AnnotationKind::Operands,
    AnnotationKind::References,
    AnnotationKind::Patches,
];

/// Changes fields of the options.
fn set_options(options: &mut DisasmOptions, item: &Value) -> Result<Vec<Value>, String> {
    let fields = item.as_object().ok_or("item must be an object")?;
    let previous = to_value(options);
    let mut merged = previous.as_object().cloned().unwrap_or_default();
    for (name, value) in fields {
        if value.is_null() {
            merged.remove(name);
        } else {
            merged.insert(name.clone(), value.clone());
        }
    }
    *options = serde_json::from_value(Value::Object(merged)).map_err(|e| e.to_string())?;
    Ok(vec![previous])
}

/// Applies `operations` to `config`, for the ROM `rom_file`, and returns
/// the annotations in canonical order: each entry names the bank holding
/// its address, and no two entries of a list have the same key. Operations
/// apply in order; `banks` and `options`, which the banks of the other
/// entries depend on, first. Errors name the operation.
pub fn apply_operations(
    config: &DisasmConfig,
    rom_file: &[u8],
    operations: &[Operation],
) -> Result<(DisasmConfig, Vec<Applied>), Vec<String>> {
    // The annotations as given: their indices are those of the file.
    let errors = config.check_format();
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut config = config.clone();
    let mut applied = Vec::new();
    let mut errors = Vec::new();
    let layout = |o: &Operation| matches!(o.kind, AnnotationKind::Banks | AnnotationKind::Options);
    let mut apply = |config: &mut DisasmConfig, index: usize, memory: Option<&Memory>| {
        let operation = &operations[index];
        let kind = operation.kind;
        let given = |text: &str| !text.trim().is_empty();
        let missing_evidence = requires_evidence(operation)
            && !operation.evidence.as_deref().is_some_and(given)
            && !operation
                .item
                .as_ref()
                .and_then(|item| item.get("evidence"))
                .and_then(Value::as_array)
                .is_some_and(|items| items.iter().any(|e| e.as_str().is_some_and(given)));
        let result = match (operation.op, kind, &operation.item, &operation.key) {
            (OperationKind::Set, AnnotationKind::Options, Some(item), None) => {
                set_options(&mut config.options, item)
            }
            (OperationKind::Set, AnnotationKind::Options, _, _) => {
                Err("set takes item (the fields of options), not key".into())
            }
            (OperationKind::Set, _, _, _) => Err("set applies only to options".into()),
            (_, AnnotationKind::Options, _, _) => Err("options are changed with set".into()),
            (OperationKind::Upsert, _, Some(_), None) if missing_evidence => {
                Err("this entry overrides what the analysis finds: give evidence".into())
            }
            (OperationKind::Upsert, _, Some(item), None) => {
                with_list!(config, kind, list => upsert(list, item, memory, operation.evidence.as_deref()))
            }
            (OperationKind::Upsert, _, _, _) => {
                Err("upsert takes item (the whole entry), not key".into())
            }
            (OperationKind::Remove, _, None, Some(key)) => {
                with_list!(config, kind, list => remove(list, kind, key, memory))
            }
            (OperationKind::Remove, _, _, _) => {
                Err("remove takes key (the entries to remove), not item".into())
            }
        };
        match result {
            Ok(previous) => applied.push(Applied { index, previous }),
            Err(e) => errors.push(format!(
                "operations[{index}] ({} {}): {e}",
                json_name(&operation.op),
                kind.name()
            )),
        }
    };
    for index in (0..operations.len()).filter(|i| layout(&operations[*i])) {
        apply(&mut config, index, None);
    }
    // Layout errors name banks entries by their index in canonical order.
    // Entries take the banks of the layout that is applied.
    sort(&mut config.banks);
    let memory = match Memory::new(rom_file, &config) {
        Ok(memory) => memory,
        Err(e) => {
            errors.push(format!("In the changed annotations: {e}"));
            return Err(errors);
        }
    };
    for kind in LIST_KINDS {
        with_list!(config, kind, list => {
            for entry in list.iter_mut() {
                // The analysis reports entries that no single bank holds.
                let _ = normalize(entry, Some(&memory));
            }
        });
    }
    for index in (0..operations.len()).filter(|i| !layout(&operations[*i])) {
        apply(&mut config, index, Some(&memory));
    }
    for kind in LIST_KINDS {
        with_list!(config, kind, list => {
            duplicates(kind, list, &mut errors);
            sort(list);
        });
    }
    // Evidence added by the operations is within the limits too.
    for error in config.check_format() {
        errors.push(format!("In the changed annotations: {error}"));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    applied.sort_by_key(|a| a.index);
    Ok((config, applied))
}

fn json_name<T: Serialize>(value: &T) -> String {
    to_value(value).as_str().unwrap_or_default().to_owned()
}

/// The text of an annotation file: fields in a fixed order, values at their
/// defaults left out, addresses as `"$C000"` and one entry per line, so that
/// the same annotations are always the same text and a changed entry is a
/// changed line.
pub fn canonical_json(config: &DisasmConfig) -> String {
    fn text<T: Serialize + ?Sized>(value: &T) -> String {
        serde_json::to_string(value).unwrap_or_default()
    }
    fn list<T: Serialize>(name: &str, items: &[T], fields: &mut Vec<String>) {
        if !items.is_empty() {
            let lines: Vec<String> = items.iter().map(|i| format!("    {}", text(i))).collect();
            fields.push(format!("  \"{name}\": [\n{}\n  ]", lines.join(",\n")));
        }
    }
    let mut fields = vec![format!(
        "  \"rom\": {}",
        text(config.rom.to_string_lossy().as_ref())
    )];
    if config.needs_format_2() {
        fields.push(format!("  \"format_version\": {}", super::FORMAT_VERSION));
    }
    if let Some(address) = config.load_address {
        fields.push(format!("  \"load_address\": {}", text(&address)));
    }
    if let Some(base) = &config.base {
        fields.push(format!(
            "  \"base\": {}",
            text(base.to_string_lossy().as_ref())
        ));
    }
    let options = text(&config.options);
    if options != "{}" {
        fields.push(format!("  \"options\": {options}"));
    }
    list("banks", &config.banks, &mut fields);
    list("entry_points", &config.entry_points, &mut fields);
    list("labels", &config.labels, &mut fields);
    list("symbols", &config.symbols, &mut fields);
    list("regions", &config.regions, &mut fields);
    list("comments", &config.comments, &mut fields);
    list("operands", &config.operands, &mut fields);
    list("references", &config.references, &mut fields);
    list("patches", &config.patches, &mut fields);
    format!("{{\n{}\n}}\n", fields.join(",\n"))
}

/// The suggestions for an issue that `fix` describes.
pub(crate) fn suggestions(
    analysis: &Analysis,
    config: &DisasmConfig,
    fix: &Fix,
) -> Vec<Suggestion> {
    let memory = &analysis.memory;
    let address = |loc: Loc| Address(memory.address(loc)).to_string();
    let upsert = |kind: AnnotationKind, item: Value| Operation {
        op: OperationKind::Upsert,
        kind,
        item: Some(item),
        key: None,
        evidence: None,
    };
    let suggestion = |description: String, operations: Vec<Operation>, needs: &[&str]| Suggestion {
        description,
        evidence_required: operations.iter().any(requires_evidence),
        operations,
        needs: needs.iter().map(|n| (*n).to_owned()).collect(),
    };
    // The entry point of a routine, with what the annotations already give it.
    let entry = |routine: Loc| -> Map<String, Value> {
        let mut fields = config
            .entry_points
            .iter()
            .find(|e| config.applies(*e) && memory.locate(e.bank, e.address).ok() == Some(routine))
            .and_then(|e| to_value(e).as_object().cloned())
            .unwrap_or_default();
        fields.insert("bank".into(), json!(routine.bank));
        fields.insert("address".into(), json!(address(routine)));
        fields
    };
    match fix {
        Fix::Layout { bank, org } => vec![suggestion(
            format!("bank {bank} is switched in at {}", Address(*org)),
            vec![upsert(
                AnnotationKind::Banks,
                json!({"bank": bank, "org": Address(*org).to_string()}),
            )],
            &[],
        )],
        Fix::Reference { from, banks } => banks
            .iter()
            .map(|bank| {
                suggestion(
                    format!("the reference is to bank {bank}"),
                    vec![upsert(
                        AnnotationKind::References,
                        json!({"bank": from.bank, "address": address(*from), "target_bank": bank}),
                    )],
                    &[],
                )
            })
            .collect(),
        Fix::SwitchBanks { routine, banks } => {
            let mut fields = entry(*routine);
            fields.remove("keeps_banks");
            let banks = banks.map(|b| b.map(|a| Address(u32::from(a)).to_string()));
            fields.insert("switch_banks".into(), json!(banks));
            vec![suggestion(
                "the routine always switches in the banks held in this RAM".into(),
                vec![upsert(AnnotationKind::EntryPoints, Value::Object(fields))],
                &[],
            )]
        }
        Fix::ReturnAddress { routine } => {
            let mut inline = entry(*routine);
            inline.remove("no_return");
            inline.insert("inline_bytes".into(), Value::Null);
            let mut no_return = entry(*routine);
            no_return.remove("inline_bytes");
            no_return.insert("no_return".into(), json!(true));
            vec![
                suggestion(
                    "the routine reads arguments after each JSR to it and returns after them"
                        .into(),
                    vec![upsert(AnnotationKind::EntryPoints, Value::Object(inline))],
                    &["inline_bytes"],
                ),
                suggestion(
                    "the routine does not return to its caller".into(),
                    vec![upsert(
                        AnnotationKind::EntryPoints,
                        Value::Object(no_return),
                    )],
                    &[],
                ),
            ]
        }
        Fix::JumpTable { start, entries } => {
            let length = if *entries > 0 {
                json!(2 * entries)
            } else {
                Value::Null
            };
            vec![suggestion(
                "the words after the JSR are a jump table".into(),
                vec![upsert(
                    AnnotationKind::Regions,
                    json!({"bank": start.bank, "start": address(*start), "length": length,
                           "type": "pointers", "code_targets": true}),
                )],
                if *entries > 0 { &[] } else { &["length"] },
            )]
        }
        Fix::Table { low, high, adjust } => {
            let mut table = json!({"bank": low.bank, "start": address(*low), "length": null,
                                   "code_targets": true});
            if *adjust != 0 {
                table["adjust"] = json!(adjust);
            }
            if high.bank == low.bank && high.offset == low.offset + 1 {
                table["type"] = json!("pointers");
                vec![suggestion(
                    "the jump targets come from a table of addresses".into(),
                    vec![upsert(AnnotationKind::Regions, table)],
                    &["length"],
                )]
            } else {
                table["type"] = json!("low_bytes");
                table["pair"] = json!(address(*high));
                let high_table = json!({"bank": high.bank, "start": address(*high),
                                        "length": null, "type": "high_bytes"});
                vec![suggestion(
                    "the jump targets come from tables of low and high bytes".into(),
                    vec![
                        upsert(AnnotationKind::Regions, table),
                        upsert(AnnotationKind::Regions, high_table),
                    ],
                    &["length"],
                )]
            }
        }
    }
}

/// The SHA-256 digest of `data` in lowercase hexadecimal.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64).wrapping_mul(8).to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (word, add) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *word = word.wrapping_add(add);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

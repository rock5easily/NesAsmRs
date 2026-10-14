//! The annotation file: what a person (or an AI reading their notes) knows
//! about a ROM, applied when it is disassembled.

#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::PathBuf;

/// A CPU address or value: a number, or a string such as `"$C000"`,
/// `"0xC000"` or `"49152"`. Reports show it as `"$C000"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address(pub u32);

impl Address {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (digits, radix) = if let Some(hex) = text.strip_prefix('$') {
            (hex, 16)
        } else if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (hex, 16)
        } else {
            (text, 10)
        };
        u32::from_str_radix(&digits.replace('_', ""), radix)
            .ok()
            .filter(|_| !digits.is_empty() && !digits.starts_with('+'))
            .map(Self)
            .ok_or_else(|| {
                format!("Invalid address '{text}'; use a number, \"$C000\" or \"0xC000\"")
            })
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "${:04X}", self.0)
    }
}

impl Serialize for Address {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(u64),
            Text(String),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Number(n) => u32::try_from(n)
                .map(Self)
                .map_err(|_| serde::de::Error::custom("address out of range")),
            Raw::Text(text) => Self::parse(&text).map_err(serde::de::Error::custom),
        }
    }
}

#[cfg(feature = "schema")]
impl JsonSchema for Address {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Address".into()
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A number, or a string such as \"$C000\", \"0xC000\" or \"49152\"",
            "anyOf": [
                { "type": "integer", "minimum": 0 },
                { "type": "string" }
            ]
        })
    }
}

/// Annotations that turn a ROM into readable, reassemblable source.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct DisasmConfig {
    /// ROM file relative to the project root: .nes with an iNES header, or a raw .bin.
    pub rom: PathBuf,
    /// The version of the annotation format: 3 when references give
    /// `target_window`, 2 when entries give `evidence` or `status` (which
    /// older versions of nesasm cannot read); absent for 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format_version: Option<u32>,
    /// For a raw .bin fragment: the CPU address of its first byte. Required
    /// when the file is not a whole number of 8 KiB banks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_address: Option<Address>,
    /// For a ROM hack: the original ROM (same format) relative to the project
    /// root. The source then includes unchanged ranges from it with `.incbin`
    /// and disassembles only the changed ranges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<PathBuf>,
    /// With `base`: ranges to disassemble besides the changed bytes, to widen
    /// the source (a whole routine, a table). Changed bytes are always included.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<PatchDef>,
    /// CPU window ($8000, $A000, $C000 or $E000) of PRG banks; others use the
    /// default layout (the last two banks at $C000/$E000, the rest alternating
    /// $8000/$A000; a single 8 KiB bank at $E000).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub banks: Vec<BankLayout>,
    /// Code entry points besides the interrupt vectors.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entry_points: Vec<EntryPoint>,
    /// Names of ROM addresses.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<LabelDef>,
    /// Names of RAM, I/O and other addresses outside ROM, emitted as `NAME = $xxxx`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<SymbolDef>,
    /// How ranges of the ROM are emitted (code, bytes, words, pointer tables, text).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<RegionDef>,
    /// Comments placed before or on the line of an address.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<CommentDef>,
    /// Operand text replacing what the disassembler would write.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operands: Vec<OperandDef>,
    /// Banks of references to addresses that several banks can hold.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<ReferenceDef>,
    pub options: DisasmOptions,
    /// Apply the hypotheses as if confirmed (see `with_hypotheses`); not
    /// part of the annotation file.
    #[serde(skip)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub apply_hypotheses: bool,
}

/// Whether an annotation is confirmed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Applied by every analysis.
    #[default]
    Confirmed,
    /// Kept with the annotations but not applied, unless an analysis asks for
    /// hypotheses (never by `export_disassembly`).
    Hypothesis,
}

/// The newest annotation format this version reads.
pub const FORMAT_VERSION: u32 = 3;
/// The CPU windows whose bank `references.target_window` can name.
pub const SWITCHABLE_WINDOWS: [u32; 2] = [0x8000, 0xa000];
/// Evidence strings per entry, and characters per string.
pub const MAX_EVIDENCE: usize = 64;
pub const MAX_EVIDENCE_TEXT: usize = 2000;

/// Runs `$body` for each annotation list of `$config`, with `$name` its
/// name and `$list` the list.
macro_rules! each_list {
    ($config:ident, $name:ident, $list:ident, [$($r:tt)+] => $body:expr) => {{
        {
            let ($name, $list) = ("banks", $($r)+ $config.banks);
            $body
        }
        {
            let ($name, $list) = ("entry_points", $($r)+ $config.entry_points);
            $body
        }
        {
            let ($name, $list) = ("labels", $($r)+ $config.labels);
            $body
        }
        {
            let ($name, $list) = ("symbols", $($r)+ $config.symbols);
            $body
        }
        {
            let ($name, $list) = ("regions", $($r)+ $config.regions);
            $body
        }
        {
            let ($name, $list) = ("comments", $($r)+ $config.comments);
            $body
        }
        {
            let ($name, $list) = ("operands", $($r)+ $config.operands);
            $body
        }
        {
            let ($name, $list) = ("references", $($r)+ $config.references);
            $body
        }
        {
            let ($name, $list) = ("patches", $($r)+ $config.patches);
            $body
        }
    }};
}

/// The `evidence` and `status` every annotation entry has.
pub(crate) trait Provenance {
    fn evidence(&self) -> &[String];
    fn evidence_mut(&mut self) -> &mut Vec<String>;
    fn status(&self) -> Status;
}

macro_rules! provenance {
    ($($t:ty),*) => {$(
        impl Provenance for $t {
            fn evidence(&self) -> &[String] {
                &self.evidence
            }
            fn evidence_mut(&mut self) -> &mut Vec<String> {
                &mut self.evidence
            }
            fn status(&self) -> Status {
                self.status
            }
        }
    )*};
}

provenance!(
    BankLayout,
    EntryPoint,
    LabelDef,
    SymbolDef,
    RegionDef,
    CommentDef,
    OperandDef,
    PatchDef,
    ReferenceDef
);

impl DisasmConfig {
    /// The number of entries that are hypotheses.
    pub fn hypotheses(&self) -> usize {
        let config = self;
        let mut count = 0;
        each_list!(config, _name, list, [&] => {
            count += list.iter().filter(|e| e.status() == Status::Hypothesis).count();
        });
        count
    }

    /// Whether an entry is a hypothesis.
    pub fn has_hypotheses(&self) -> bool {
        self.hypotheses() > 0
    }

    /// The annotations with the hypotheses applied as if confirmed, to see
    /// what they would change. The entries keep their status (and indices).
    pub fn with_hypotheses(&self) -> Self {
        Self {
            apply_hypotheses: true,
            ..self.clone()
        }
    }

    /// Whether an analysis applies `entry`: a confirmed one, or any with
    /// `apply_hypotheses`.
    pub(crate) fn applies<T: Provenance>(&self, entry: &T) -> bool {
        self.apply_hypotheses || entry.status() == Status::Confirmed
    }

    /// The format version the annotations need: 3 when a reference gives
    /// `target_window`, 2 when an entry gives `evidence` or `status`.
    pub(crate) fn required_format(&self) -> Option<u32> {
        if self.references.iter().any(|r| r.target_window.is_some()) {
            return Some(3);
        }
        let config = self;
        let mut found = false;
        each_list!(config, _name, list, [&] => {
            found |= list
                .iter()
                .any(|e| !e.evidence().is_empty() || e.status() != Status::Confirmed);
        });
        found.then_some(2)
    }

    /// Errors in the format version and the evidence of the entries.
    pub(crate) fn check_format(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if let Some(version) = self.format_version
            && !(1..=FORMAT_VERSION).contains(&version)
        {
            errors.push(format!(
                "format_version {version} is not supported: this version of nesasm reads annotations up to format_version {FORMAT_VERSION}; use a newer nesasm-mcp"
            ));
        }
        let config = self;
        each_list!(config, name, list, [&] => {
            for (i, entry) in list.iter().enumerate() {
                let evidence = entry.evidence();
                if evidence.len() > MAX_EVIDENCE {
                    errors.push(format!(
                        "{name}[{i}]: evidence has {} items; at most {MAX_EVIDENCE}",
                        evidence.len()
                    ));
                }
                if let Some(text) = evidence.iter().find(|t| t.chars().count() > MAX_EVIDENCE_TEXT) {
                    errors.push(format!(
                        "{name}[{i}]: evidence of {} characters; at most {MAX_EVIDENCE_TEXT}",
                        text.chars().count()
                    ));
                }
            }
        });
        for (i, reference) in self.references.iter().enumerate() {
            if let Err(e) = reference.check() {
                errors.push(format!("references[{i}] {}: {e}", reference.address));
            }
        }
        errors
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BankLayout {
    pub bank: usize,
    /// CPU address of the bank's first byte, a multiple of $2000.
    pub org: Address,
    /// What was observed that supports the entry (callers, the code that
    /// reads a table, documentation), one observation per string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// `hypothesis`: not confirmed yet; the analysis applies it only on request.
    #[serde(default, skip_serializing_if = "is_default")]
    pub status: Status,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EntryPoint {
    /// Needed only when several banks hold the address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<usize>,
    pub address: Address,
    /// Label name (default: an automatic name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// The routine never returns to its caller, so code after a JSR to it is not followed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_return: bool,
    /// Bytes of inline arguments after each JSR to the routine; they are emitted as data.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub inline_bytes: usize,
    /// The routine switches in the PRG bank held in the first RAM address
    /// at $8000 and the one in the second at $A000 (`null`: not switched),
    /// and changes no other RAM the caller knows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switch_banks: Option<[Option<Address>; 2]>,
    /// The routine returns with the PRG banks it was called with (such as
    /// a thread yield that other threads' bank switches run during).
    #[serde(default, skip_serializing_if = "is_false")]
    pub keeps_banks: bool,
    /// What was observed that supports the entry (callers, the code that
    /// reads a table, documentation), one observation per string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// `hypothesis`: not confirmed yet; the analysis applies it only on request.
    #[serde(default, skip_serializing_if = "is_default")]
    pub status: Status,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LabelDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<usize>,
    pub address: Address,
    pub name: String,
    /// Comment placed above the label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// What was observed that supports the entry (callers, the code that
    /// reads a table, documentation), one observation per string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// `hypothesis`: not confirmed yet; the analysis applies it only on request.
    #[serde(default, skip_serializing_if = "is_default")]
    pub status: Status,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SymbolDef {
    pub name: String,
    pub value: Address,
    /// Bytes covered (default 1); operands inside are written `name+offset`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// What was observed that supports the entry (callers, the code that
    /// reads a table, documentation), one observation per string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// `hypothesis`: not confirmed yet; the analysis applies it only on request.
    #[serde(default, skip_serializing_if = "is_default")]
    pub status: Status,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RegionType {
    /// Decode every instruction in the range, even if no flow reaches it.
    Code,
    /// `.db` values.
    #[default]
    Bytes,
    /// `.dw` values.
    Words,
    /// `.dw` addresses, written as labels.
    Pointers,
    /// Low bytes of addresses whose high bytes are the `pair` table; `.db LOW(label)`.
    LowBytes,
    /// High bytes of addresses whose low bytes are the `pair` table; `.db HIGH(label)`.
    HighBytes,
    /// ASCII text as `.db "..."`; other bytes as values.
    Text,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RegionDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<usize>,
    pub start: Address,
    /// Last address (inclusive); give this or `length`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<Address>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<u32>,
    #[serde(rename = "type", default, skip_serializing_if = "is_default")]
    pub kind: RegionType,
    /// For low_bytes/high_bytes: start of the other table (same length and bank).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pair: Option<Address>,
    /// For pointer tables: the targets are code entry points (jump tables).
    #[serde(default, skip_serializing_if = "is_false")]
    pub code_targets: bool,
    /// For pointer tables: stored value minus target, as in `.dw label-1`
    /// for RTS jump tables (-1).
    #[serde(default, skip_serializing_if = "is_default")]
    pub adjust: i32,
    /// Values per line (default `options.data_per_line`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<usize>,
    /// Comment placed above the region.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// For pointer and low_bytes tables: start of a table of the same
    /// number of entries (and bank) holding the PRG bank (8 KiB) switched in
    /// for each entry. A target that bank holds is resolved to it; others
    /// (in the fixed banks, or the table's own bank) as usual.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank_table: Option<Address>,
    /// What was observed that supports the entry (callers, the code that
    /// reads a table, documentation), one observation per string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// `hypothesis`: not confirmed yet; the analysis applies it only on request.
    #[serde(default, skip_serializing_if = "is_default")]
    pub status: Status,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize,
)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CommentPosition {
    /// At the end of the line.
    #[default]
    Line,
    /// On lines of their own before the address (and its labels).
    Above,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CommentDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<usize>,
    pub address: Address,
    pub text: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub position: CommentPosition,
    /// What was observed that supports the entry (callers, the code that
    /// reads a table, documentation), one observation per string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// `hypothesis`: not confirmed yet; the analysis applies it only on request.
    #[serde(default, skip_serializing_if = "is_default")]
    pub status: Status,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct OperandDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<usize>,
    /// Address of the instruction or data value.
    pub address: Address,
    /// The operand as written after the mnemonic (`#LOW(Table)`, `<ptr+1`,
    /// `[ptr],y`) or the data value (`Table-1`). The round-trip check verifies it.
    pub expr: String,
    /// What was observed that supports the entry (callers, the code that
    /// reads a table, documentation), one observation per string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// `hypothesis`: not confirmed yet; the analysis applies it only on request.
    #[serde(default, skip_serializing_if = "is_default")]
    pub status: Status,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PatchDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<usize>,
    pub start: Address,
    /// Last address (inclusive); give this or `length`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<Address>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<u32>,
    /// Comment placed above the range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// What was observed that supports the entry (callers, the code that
    /// reads a table, documentation), one observation per string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// `hypothesis`: not confirmed yet; the analysis applies it only on request.
    #[serde(default, skip_serializing_if = "is_default")]
    pub status: Status,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReferenceDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<usize>,
    /// Address of the referring instruction or pointer.
    pub address: Address,
    /// Bank holding the referenced address. Give this or `target_window`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_bank: Option<usize>,
    /// For an instruction that reads data from whichever PRG bank is
    /// switched in at run time (tables of the same layout in several
    /// banks): the CPU window, "$8000" or "$A000", that holds the operand
    /// address. The reference is not fixed to a bank: the source keeps the
    /// address as a number, and the bytes of no bank are taken as the value
    /// read. Only for reads with an absolute operand (`lda $AD00,y`), not
    /// writes, jumps or calls. Needs `evidence` and format_version 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_window: Option<Address>,
    /// What was observed that supports the entry (callers, the code that
    /// reads a table, documentation), one observation per string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// `hypothesis`: not confirmed yet; the analysis applies it only on request.
    #[serde(default, skip_serializing_if = "is_default")]
    pub status: Status,
}

impl ReferenceDef {
    /// Errors of the entry by itself: one of `target_bank` and
    /// `target_window`, a switchable window, evidence for a window.
    pub(crate) fn check(&self) -> Result<(), String> {
        match (self.target_bank, self.target_window) {
            (Some(_), Some(_)) => Err(
                "give target_bank (a fixed bank) or target_window (the bank switched in at run time), not both".into(),
            ),
            (None, None) => Err("give target_bank or target_window".into()),
            (None, Some(window)) if !SWITCHABLE_WINDOWS.contains(&window.0) => Err(format!(
                "target_window {window} is not a switchable window; give \"$8000\" or \"$A000\""
            )),
            (None, Some(_)) if !self.evidence.iter().any(|e| !e.trim().is_empty()) => Err(
                "target_window needs evidence: how the bank is switched in and why it varies".into(),
            ),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChrMode {
    /// `.incbin "<output stem>.chr"`, written next to the source.
    #[default]
    Incbin,
    /// `.db` lines in the source.
    Db,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct DisasmOptions {
    /// Upper-case mnemonics (default lower case).
    #[serde(skip_serializing_if = "is_false")]
    pub uppercase: bool,
    /// Values per `.db`/`.dw` line (default 8).
    #[serde(skip_serializing_if = "is_default_data_per_line")]
    pub data_per_line: usize,
    /// Append the CPU address of each line as a comment.
    #[serde(skip_serializing_if = "is_false")]
    pub address_comments: bool,
    /// Name RAM addresses automatically (`zp_10`, `ram_0300`).
    #[serde(skip_serializing_if = "is_false")]
    pub auto_ram_labels: bool,
    /// Name the PPU, APU and I/O registers (PPUCTRL ... JOY2; default true).
    #[serde(skip_serializing_if = "is_true")]
    pub nes_registers: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub chr: ChrMode,
    /// Write each bank to `<stem>_bankNN.asm`, included from the main file.
    /// By default, only when the source would exceed 512 KiB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split_banks: Option<bool>,
    /// Mapper 4 (MMC3) only: the second-to-last PRG bank is at $C000 and the
    /// last at $E000, and $C000-$FFFF is analysed as one range, so code runs
    /// across $DFFF/$E000. With `split_banks`, both are written to
    /// `<stem>_bankNN_MM.asm`.
    #[serde(skip_serializing_if = "is_false")]
    pub mapper4_fixed_prg_16k: bool,
}

impl Default for DisasmOptions {
    fn default() -> Self {
        Self {
            uppercase: false,
            data_per_line: 8,
            address_comments: false,
            auto_ram_labels: false,
            nes_registers: true,
            chr: ChrMode::Incbin,
            split_banks: None,
            mapper4_fixed_prg_16k: false,
        }
    }
}

// Fields at their default values are left out when annotations are written.
fn is_false(value: &bool) -> bool {
    !*value
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

fn is_default_data_per_line(value: &usize) -> bool {
    *value == DisasmOptions::default().data_per_line
}

/// Standard names of the NES registers.
pub(crate) const NES_REGISTERS: [(&str, u32); 25] = [
    ("PPUCTRL", 0x2000),
    ("PPUMASK", 0x2001),
    ("PPUSTATUS", 0x2002),
    ("OAMADDR", 0x2003),
    ("OAMDATA", 0x2004),
    ("PPUSCROLL", 0x2005),
    ("PPUADDR", 0x2006),
    ("PPUDATA", 0x2007),
    ("SQ1_VOL", 0x4000),
    ("SQ1_SWEEP", 0x4001),
    ("SQ1_LO", 0x4002),
    ("SQ1_HI", 0x4003),
    ("SQ2_VOL", 0x4004),
    ("SQ2_SWEEP", 0x4005),
    ("SQ2_LO", 0x4006),
    ("SQ2_HI", 0x4007),
    ("TRI_LINEAR", 0x4008),
    ("TRI_LO", 0x400a),
    ("TRI_HI", 0x400b),
    ("NOISE_VOL", 0x400c),
    ("NOISE_LO", 0x400e),
    ("NOISE_HI", 0x400f),
    ("OAMDMA", 0x4014),
    ("SND_CHN", 0x4015),
    ("JOY1", 0x4016),
];

/// $4017 (JOY2 when read, the APU frame counter when written).
pub(crate) const JOY2: (&str, u32) = ("JOY2", 0x4017);

/// Names that cannot be labels or symbols in the generated source.
const RESERVED: [&str; 21] = [
    "A",
    "X",
    "Y",
    "LOW",
    "HIGH",
    "BANK",
    "PAGE",
    "DEFINED",
    "SIZEOF",
    "VRAM",
    "PAL",
    "REGIONSIZE",
    "LOW_BYTE",
    "HIGH_BYTE",
    "MAGICKIT",
    "DEVELO",
    "CDROM",
    "_BSS_END",
    "_BANK_BASE",
    "_NB_BANK",
    "_CALL_BANK",
];

/// Checks a label or symbol name for the generated source.
pub(crate) fn check_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    if !valid {
        return Err(format!(
            "Invalid name '{name}': use letters, digits and _, not starting with a digit, up to 64 characters"
        ));
    }
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(name)) {
        return Err(format!("Name '{name}' is reserved by the assembler"));
    }
    Ok(())
}

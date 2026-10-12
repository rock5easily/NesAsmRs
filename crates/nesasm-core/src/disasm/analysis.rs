//! The ROM as banks of CPU addresses, code-flow tracing from the entry
//! points, references and names.

use super::config::{
    self, Address, CommentPosition, DisasmConfig, JOY2, NES_REGISTERS, RegionType,
};
use super::{Issue, IssueKind, RomFormat, XrefKind};
use crate::inspect::{self, HEADER_SIZE, PRG_UNIT, Rom, VECTORS};
use crate::opcode::{Mnemonic, Mode};
use crate::state::{BANK_SIZE, MAX_BANKS};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};

/// Instructions followed per routine for the routine report.
pub(crate) const MAX_ROUTINE: usize = 16384;

/// Runs of one value at least this long are padding (`.ds`), not unknown data.
pub(crate) const FILL_RUN: usize = 16;

/// A byte of a bank: its bank and offset within the bank.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Loc {
    pub bank: usize,
    pub offset: usize,
}

impl Loc {
    pub fn at(self, offset: usize) -> Self {
        Self { offset, ..self }
    }
}

/// One 8 KiB bank of the ROM.
pub(crate) struct BankImage {
    pub index: usize,
    pub chr: bool,
    /// CPU address of offset 0 (PRG banks).
    pub org: u32,
    /// The bank's bytes, padded to 8 KiB.
    pub data: Vec<u8>,
    /// Offsets holding ROM data (a fragment may cover part of a bank).
    pub start: usize,
    pub end: usize,
}

impl BankImage {
    fn holds(&self, address: u32) -> bool {
        !self.chr && address >= self.org + self.start as u32 && address < self.org + self.end as u32
    }
}

pub(crate) struct Memory {
    pub format: RomFormat,
    /// The 16 iNES header bytes, empty for raw files.
    pub header: Vec<u8>,
    pub trainer: bool,
    /// The trainer's bytes, which the source cannot reproduce.
    pub trainer_bytes: Vec<u8>,
    pub banks: Vec<BankImage>,
    /// Bytes of ROM data (without header and trainer).
    pub payload_len: usize,
}

/// Where a CPU address points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    Rom(Loc),
    /// Several banks hold the address.
    Ambiguous(Vec<usize>),
    /// No PRG bank holds the address (RAM, I/O, SRAM).
    Outside,
}

/// Default CPU windows of `count` PRG banks: the last two at $C000/$E000,
/// the others alternating $8000/$A000; a single bank at $E000.
fn default_org(bank: usize, count: usize) -> u32 {
    if bank + 1 == count {
        0xe000
    } else if bank + 2 == count {
        0xc000
    } else if bank.is_multiple_of(2) {
        0x8000
    } else {
        0xa000
    }
}

impl Memory {
    pub fn new(file: &[u8], config: &DisasmConfig) -> Result<Self, String> {
        let rom = Rom::from_file(file)?;
        let header = rom.header_bytes().to_vec();
        let payload = &rom.payload;
        if payload.is_empty() {
            return Err("The ROM has no data".into());
        }
        let (format, prg_count) = match (&rom.header, config.load_address) {
            (Some(_), Some(_)) => {
                return Err("load_address applies only to raw .bin files".into());
            }
            (Some(h), None) => (
                RomFormat::Nes,
                (usize::from(h.prg_16k) * PRG_UNIT / BANK_SIZE)
                    .min(payload.len().div_ceil(BANK_SIZE)),
            ),
            (None, Some(_)) => (RomFormat::Fragment, 0),
            (None, None) if payload.len() % BANK_SIZE == 0 => {
                (RomFormat::Bin, payload.len() / BANK_SIZE)
            }
            (None, None) => {
                return Err(format!(
                    "The file has {} bytes, not a whole number of 8 KiB banks; give load_address for a fragment",
                    payload.len()
                ));
            }
        };
        let mut banks = Vec::new();
        if let Some(load) = config.load_address {
            if !config.banks.is_empty() {
                return Err("banks cannot be combined with load_address".into());
            }
            let load = load.0;
            let base = load & !(BANK_SIZE as u32 - 1);
            let first = load as usize - base as usize;
            let count = (first + payload.len()).div_ceil(BANK_SIZE);
            if load > 0xffff || base as usize + count * BANK_SIZE > 0x10000 {
                return Err("The fragment runs past $FFFF".into());
            }
            for index in 0..count {
                let begin = (index * BANK_SIZE).saturating_sub(first);
                let start = if index == 0 { first } else { 0 };
                let end = (first + payload.len() - index * BANK_SIZE).min(BANK_SIZE);
                let mut data = vec![0; BANK_SIZE];
                data[start..end].copy_from_slice(&payload[begin..begin + end - start]);
                banks.push(BankImage {
                    index,
                    chr: false,
                    org: base + (index * BANK_SIZE) as u32,
                    data,
                    start,
                    end,
                });
            }
        } else {
            let count = payload.len().div_ceil(BANK_SIZE);
            if count > MAX_BANKS {
                return Err(format!(
                    "The ROM has {count} banks; at most {MAX_BANKS} are supported"
                ));
            }
            for (index, chunk) in payload.chunks(BANK_SIZE).enumerate() {
                let mut data = chunk.to_vec();
                data.resize(BANK_SIZE, 0);
                banks.push(BankImage {
                    index,
                    chr: index >= prg_count,
                    org: if index < prg_count {
                        default_org(index, prg_count)
                    } else {
                        0
                    },
                    data,
                    start: 0,
                    end: chunk.len(),
                });
            }
            for layout in &config.banks {
                let org = layout.org.0;
                if org % BANK_SIZE as u32 != 0 || org > 0xe000 {
                    return Err(format!(
                        "Bank {} org {} must be a multiple of $2000 up to $E000",
                        layout.bank, layout.org
                    ));
                }
                match banks.get_mut(layout.bank) {
                    Some(bank) if !bank.chr => bank.org = org,
                    _ => return Err(format!("Bank {} is not a PRG bank of the ROM", layout.bank)),
                }
            }
        }
        Ok(Self {
            format,
            header,
            trainer: rom.header.as_ref().is_some_and(|h| h.trainer),
            trainer_bytes: if rom.header.as_ref().is_some_and(|h| h.trainer) {
                file.get(HEADER_SIZE..HEADER_SIZE + 512)
                    .unwrap_or_default()
                    .to_vec()
            } else {
                Vec::new()
            },
            banks,
            payload_len: payload.len(),
        })
    }

    pub fn address(&self, loc: Loc) -> u32 {
        self.banks[loc.bank].org + loc.offset as u32
    }

    pub fn byte(&self, loc: Loc) -> u8 {
        self.banks[loc.bank].data[loc.offset]
    }

    /// PRG banks holding `address`.
    pub fn holders(&self, address: u32) -> Vec<usize> {
        self.banks
            .iter()
            .filter(|b| b.holds(address))
            .map(|b| b.index)
            .collect()
    }

    /// Where `address` points, seen from `from` (whose own window wins).
    pub fn resolve(&self, from: Option<usize>, address: u32) -> Resolved {
        if let Some(bank) = from.map(|b| &self.banks[b])
            && bank.holds(address)
        {
            return Resolved::Rom(Loc {
                bank: bank.index,
                offset: (address - bank.org) as usize,
            });
        }
        match self.holders(address).as_slice() {
            [] => Resolved::Outside,
            [bank] => Resolved::Rom(Loc {
                bank: *bank,
                offset: (address - self.banks[*bank].org) as usize,
            }),
            several => Resolved::Ambiguous(several.to_vec()),
        }
    }

    /// The location an annotation names.
    pub fn locate(&self, bank: Option<usize>, address: Address) -> Result<Loc, String> {
        let address = address.0;
        match bank {
            Some(index) => match self.banks.get(index) {
                Some(b) if b.holds(address) => Ok(Loc {
                    bank: index,
                    offset: (address - b.org) as usize,
                }),
                Some(b) if !b.chr => Err(format!(
                    "Bank {index} (${:04X}-${:04X}) does not hold ${address:04X}",
                    b.org + b.start as u32,
                    b.org + b.end as u32 - 1
                )),
                _ => Err(format!("Bank {index} is not a PRG bank of the ROM")),
            },
            None => match self.resolve(None, address) {
                Resolved::Rom(loc) => Ok(loc),
                Resolved::Ambiguous(banks) => Err(format!(
                    "Banks {} hold ${address:04X}; give bank",
                    join(&banks)
                )),
                Resolved::Outside => Err(format!("No PRG bank holds ${address:04X}")),
            },
        }
    }

    /// The start and end offset of a range an annotation names by start and
    /// end (inclusive) or length; it must lie in one bank.
    pub fn span(
        &self,
        bank: Option<usize>,
        start: Address,
        end: Option<Address>,
        length: Option<u32>,
    ) -> Result<(Loc, usize), String> {
        let loc = self.locate(bank, start)?;
        let length = match (end, length) {
            (Some(end), None) if end.0 >= start.0 => u64::from(end.0 - start.0) + 1,
            (None, Some(length)) if length > 0 => u64::from(length),
            (Some(_), None) => return Err("end is before start".into()),
            _ => return Err("give either end or a nonzero length".into()),
        };
        let bank = &self.banks[loc.bank];
        if loc.offset as u64 + length > bank.end as u64 {
            return Err(format!(
                "the range runs past the end of bank {} (${:04X})",
                loc.bank,
                bank.org + bank.end as u32 - 1
            ));
        }
        Ok((loc, loc.offset + length as usize))
    }

    pub fn prg_banks(&self) -> impl Iterator<Item = &BankImage> {
        self.banks.iter().filter(|b| !b.chr)
    }
}

fn join(banks: &[usize]) -> String {
    banks
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Unknown,
    /// First byte of an instruction.
    Insn,
    /// Operand byte of an instruction.
    Operand,
    Data,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Insn {
    pub mnemonic: Mnemonic,
    pub mode: Mode,
    pub len: usize,
    /// Operand value; the target address for branches.
    pub value: u32,
}

/// A range of one bank emitted in one way.
#[derive(Clone, Debug)]
pub(crate) struct Region {
    pub bank: usize,
    pub start: usize,
    pub end: usize,
    pub kind: RegionType,
    /// For low/high byte tables: offset of the other table.
    pub pair: Option<usize>,
    pub code_targets: bool,
    pub adjust: i32,
    pub columns: Option<usize>,
    pub comment: Option<String>,
}

/// What a reference points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Rom(Loc),
    Ambiguous(u32),
    Outside(u32),
}

#[derive(Clone, Debug)]
pub(crate) struct Ref {
    pub from: Loc,
    pub kind: XrefKind,
    pub target: Target,
}

/// Kinds of automatic labels, in increasing priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Want {
    Data,
    Jump,
    Call,
}

#[derive(Clone, Debug)]
pub(crate) struct Label {
    pub name: String,
    pub comment: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Routine {
    no_return: bool,
    inline_bytes: usize,
}

/// A named address outside the ROM, emitted as `NAME = $xxxx`.
#[derive(Clone, Debug)]
pub(crate) struct SymbolEntry {
    pub name: String,
    pub value: u32,
    pub size: u32,
    pub comment: Option<String>,
    /// Defined by the annotations (always emitted).
    pub user: bool,
}

pub(crate) struct Analysis {
    pub memory: Memory,
    pub class: Vec<Vec<Class>>,
    pub insns: BTreeMap<Loc, Insn>,
    /// Regions of each bank, sorted by start.
    pub regions: Vec<Vec<Region>>,
    pub labels: BTreeMap<Loc, Vec<Label>>,
    pub refs: Vec<Ref>,
    pub issues: Vec<Issue>,
    pub symbols: Vec<SymbolEntry>,
    pub comments: BTreeMap<Loc, (Vec<String>, Vec<String>)>,
    pub operands: HashMap<Loc, String>,
    /// Routine entries (calls, vectors, entry points) and the instructions of each.
    pub routines: BTreeMap<Loc, Vec<Loc>>,
    pub options: config::DisasmOptions,
    pub vectors: BTreeMap<Loc, &'static str>,
    /// The routine of each instruction in a routine body (the first routine found).
    pub routine_index: HashMap<Loc, Vec<Loc>>,
    /// Routines whose code exceeds [`MAX_ROUTINE`] instructions.
    pub truncated: HashSet<Loc>,
    references: HashMap<Loc, usize>,
    entries: HashMap<Loc, Routine>,
    /// ROM targets of calls, jumps and branches.
    flow: HashMap<Loc, Loc>,
    wanted: BTreeMap<Loc, Want>,
    /// Code to trace, in the order found.
    work: VecDeque<(Loc, Option<Loc>)>,
    decode: [Option<(Mnemonic, Mode)>; 256],
}

impl Analysis {
    pub fn run(
        file: &[u8],
        config: &DisasmConfig,
        cancel: &AtomicBool,
    ) -> Result<Self, Vec<String>> {
        let memory = Memory::new(file, config).map_err(|e| vec![e])?;
        let class = memory
            .banks
            .iter()
            .map(|_| vec![Class::Unknown; BANK_SIZE])
            .collect();
        let regions = memory.banks.iter().map(|_| Vec::new()).collect();
        let mut analysis = Self {
            memory,
            class,
            insns: BTreeMap::new(),
            regions,
            labels: BTreeMap::new(),
            refs: Vec::new(),
            issues: Vec::new(),
            symbols: Vec::new(),
            comments: BTreeMap::new(),
            operands: HashMap::new(),
            routines: BTreeMap::new(),
            options: config.options.clone(),
            vectors: BTreeMap::new(),
            routine_index: HashMap::new(),
            truncated: HashSet::new(),
            references: HashMap::new(),
            entries: HashMap::new(),
            flow: HashMap::new(),
            wanted: BTreeMap::new(),
            work: VecDeque::new(),
            decode: inspect::decode_table(),
        };
        analysis.apply(config)?;
        analysis.check_header();
        analysis.trace_all(cancel)?;
        analysis.name_labels()?;
        analysis.find_routines();
        analysis.report_unknown();
        Ok(analysis)
    }

    fn issue(&mut self, kind: IssueKind, loc: Option<Loc>, address: u32, message: String) {
        self.issues.push(Issue {
            kind,
            bank: loc.map(|l| l.bank),
            address: Address(address),
            message,
            spans: loc.map(|l| (l.offset, 1)).into_iter().collect(),
        });
    }

    /// Applies the annotations, collecting every error.
    fn apply(&mut self, config: &DisasmConfig) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if self.options.data_per_line == 0 {
            errors.push("options.data_per_line must be at least 1".into());
        }
        let mut names: HashMap<String, String> = HashMap::new();
        let mut claim = |name: &str, what: String, errors: &mut Vec<String>| {
            if let Err(e) = config::check_name(name) {
                errors.push(e);
            } else if let Some(other) = names.insert(name.to_owned(), what.clone()) {
                errors.push(format!("Name '{name}' is used by {other} and {what}"));
            }
        };
        for entry in &config.entry_points {
            if entry.inline_bytes > BANK_SIZE {
                errors.push(format!(
                    "entry_points {}: inline_bytes must be at most {BANK_SIZE}",
                    entry.address
                ));
                continue;
            }
            match self.memory.locate(entry.bank, entry.address) {
                Ok(loc) => {
                    if let Some(name) = &entry.name {
                        claim(name, format!("entry point {}", entry.address), &mut errors);
                        self.add_label(loc, name, entry.comment.clone());
                    } else if let Some(comment) = &entry.comment {
                        self.comment(loc, comment, CommentPosition::Above);
                    }
                    self.entries.insert(
                        loc,
                        Routine {
                            no_return: entry.no_return,
                            inline_bytes: entry.inline_bytes,
                        },
                    );
                    self.work.push_back((loc, None));
                    self.want(loc, Want::Call);
                }
                Err(e) => errors.push(format!("entry_points {}: {e}", entry.address)),
            }
        }
        for label in &config.labels {
            match self.memory.locate(label.bank, label.address) {
                Ok(loc) => {
                    claim(&label.name, format!("label {}", label.address), &mut errors);
                    self.add_label(loc, &label.name, label.comment.clone());
                }
                Err(e) => errors.push(format!("labels {} ({}): {e}", label.address, label.name)),
            }
        }
        for symbol in &config.symbols {
            claim(
                &symbol.name,
                format!("symbol {}", symbol.value),
                &mut errors,
            );
            if symbol.value.0 > 0xffff {
                errors.push(format!(
                    "Symbol {} value {} exceeds $FFFF",
                    symbol.name, symbol.value
                ));
            }
            if symbol.size == Some(0) {
                errors.push(format!("Symbol {} size must be at least 1", symbol.name));
            }
            if u64::from(symbol.value.0) + u64::from(symbol.size.unwrap_or(1)) > 0x10000 {
                errors.push(format!(
                    "Symbol {} covers addresses past $FFFF",
                    symbol.name
                ));
                continue;
            }
            self.symbols.push(SymbolEntry {
                name: symbol.name.clone(),
                value: symbol.value.0,
                size: symbol.size.unwrap_or(1),
                comment: symbol.comment.clone(),
                user: true,
            });
        }
        if self.options.nes_registers {
            for (name, value) in NES_REGISTERS.into_iter().chain([JOY2]) {
                if !names.contains_key(name) && !self.symbols.iter().any(|s| s.value == value) {
                    self.symbols.push(SymbolEntry {
                        name: name.into(),
                        value,
                        size: 1,
                        comment: None,
                        user: false,
                    });
                }
            }
        }
        for comment in &config.comments {
            match self.memory.locate(comment.bank, comment.address) {
                Ok(loc) => self.comment(loc, &comment.text, comment.position),
                Err(e) => errors.push(format!("comments {}: {e}", comment.address)),
            }
        }
        for operand in &config.operands {
            match self.memory.locate(operand.bank, operand.address) {
                Ok(_) if operand.expr.trim().is_empty() || operand.expr.contains(['\n', ';']) => {
                    errors.push(format!(
                        "operands {}: the expression must be one line without a comment",
                        operand.address
                    ));
                }
                Ok(loc) => {
                    self.operands.insert(loc, operand.expr.trim().to_owned());
                }
                Err(e) => errors.push(format!("operands {}: {e}", operand.address)),
            }
        }
        for reference in &config.references {
            match self.memory.locate(reference.bank, reference.address) {
                Ok(loc)
                    if self
                        .memory
                        .banks
                        .get(reference.target_bank)
                        .is_some_and(|b| !b.chr) =>
                {
                    self.references.insert(loc, reference.target_bank);
                }
                Ok(_) => errors.push(format!(
                    "references {}: bank {} is not a PRG bank of the ROM",
                    reference.address, reference.target_bank
                )),
                Err(e) => errors.push(format!("references {}: {e}", reference.address)),
            }
        }
        self.apply_regions(config, &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn apply_regions(&mut self, config: &DisasmConfig, errors: &mut Vec<String>) {
        for def in &config.regions {
            let what = format!("regions {}", def.start);
            let (loc, end) = match self.memory.span(def.bank, def.start, def.end, def.length) {
                Ok(span) => span,
                Err(e) => {
                    errors.push(format!("{what}: {e}"));
                    continue;
                }
            };
            let pair = match (def.kind, def.pair) {
                (RegionType::LowBytes | RegionType::HighBytes, Some(pair)) => {
                    match self.memory.locate(Some(loc.bank), pair) {
                        Ok(p) => Some(p.offset),
                        Err(e) => {
                            errors.push(format!("{what}: pair {e}"));
                            continue;
                        }
                    }
                }
                (RegionType::LowBytes, None) => {
                    errors.push(format!(
                        "{what}: low_bytes needs pair (the high_bytes table)"
                    ));
                    continue;
                }
                (RegionType::HighBytes, None) => None,
                (_, Some(_)) => {
                    errors.push(format!(
                        "{what}: pair applies only to low_bytes and high_bytes"
                    ));
                    continue;
                }
                (_, None) => None,
            };
            if def.columns == Some(0) {
                errors.push(format!("{what}: columns must be at least 1"));
            }
            self.regions[loc.bank].push(Region {
                bank: loc.bank,
                start: loc.offset,
                end,
                kind: def.kind,
                pair,
                code_targets: def.code_targets,
                adjust: def.adjust,
                columns: def.columns,
                comment: def.comment.clone(),
            });
        }
        for (bank, regions) in self.regions.iter_mut().enumerate() {
            regions.sort_by_key(|r| r.start);
            for pair in regions.windows(2) {
                if pair[1].start < pair[0].end {
                    let org = self.memory.banks[bank].org;
                    errors.push(format!(
                        "Regions at ${:04X} and ${:04X} in bank {bank} overlap",
                        org + pair[0].start as u32,
                        org + pair[1].start as u32
                    ));
                }
            }
        }
        // Each low byte table names its high byte table of the same length.
        for regions in &self.regions {
            for low in regions.iter().filter(|r| r.kind == RegionType::LowBytes) {
                let org = self.memory.banks[low.bank].org;
                let matching = regions.iter().find(|r| {
                    r.kind == RegionType::HighBytes
                        && Some(r.start) == low.pair
                        && r.end - r.start == low.end - low.start
                        && r.pair.is_none_or(|p| p == low.start)
                });
                if matching.is_none() {
                    errors.push(format!(
                        "regions ${:04X}: pair must be the start of a high_bytes region of the same length",
                        org + low.start as u32
                    ));
                }
            }
            for high in regions.iter().filter(|r| r.kind == RegionType::HighBytes) {
                if !regions
                    .iter()
                    .any(|r| r.kind == RegionType::LowBytes && r.pair == Some(high.start))
                {
                    let org = self.memory.banks[high.bank].org;
                    errors.push(format!(
                        "regions ${:04X}: no low_bytes region names this high_bytes table as its pair",
                        org + high.start as u32
                    ));
                }
            }
        }
        if !errors.is_empty() {
            return;
        }
        for regions in &self.regions {
            for region in regions.iter().filter(|r| r.kind != RegionType::Code) {
                self.class[region.bank][region.start..region.end].fill(Class::Data);
            }
        }
    }

    fn add_label(&mut self, loc: Loc, name: &str, comment: Option<String>) {
        self.labels.entry(loc).or_default().push(Label {
            name: name.to_owned(),
            comment,
        });
    }

    fn comment(&mut self, loc: Loc, text: &str, position: CommentPosition) {
        let entry = self.comments.entry(loc).or_default();
        match position {
            CommentPosition::Above => entry.0.extend(text.lines().map(str::to_owned)),
            CommentPosition::Line => entry
                .1
                .push(text.split_whitespace().collect::<Vec<_>>().join(" ")),
        }
    }

    fn want(&mut self, loc: Loc, want: Want) {
        let entry = self.wanted.entry(loc).or_insert(want);
        *entry = (*entry).max(want);
    }

    /// Header fields the assembler cannot reproduce.
    fn check_header(&mut self) {
        let header = self.memory.header.clone();
        if header.len() == HEADER_SIZE {
            if header[7] & 0x0f != 0 || header[8..].iter().any(|&b| b != 0) {
                self.issue(
                    IssueKind::Header,
                    None,
                    0,
                    "Header bytes 7 (low nibble) and 8-15 are not zero (NES 2.0 or extra fields); NESASM writes them as zero".into(),
                );
            }
            if self.memory.trainer {
                self.issue(
                    IssueKind::Header,
                    None,
                    0,
                    "The ROM has a 512-byte trainer, which NESASM cannot produce".into(),
                );
            }
            let declared = usize::from(header[4]) * PRG_UNIT + usize::from(header[5]) * BANK_SIZE;
            if declared != self.memory.payload_len {
                self.issue(
                    IssueKind::Header,
                    None,
                    0,
                    format!(
                        "The header declares {declared} bytes of PRG+CHR ROM, the file has {}",
                        self.memory.payload_len
                    ),
                );
            }
        }
        if !self.memory.payload_len.is_multiple_of(BANK_SIZE)
            && self.memory.format != RomFormat::Fragment
        {
            self.issue(
                IssueKind::Header,
                None,
                0,
                "The ROM data is not a whole number of 8 KiB banks; NESASM pads the last bank"
                    .into(),
            );
        }
    }

    /// The bank a reference from `from` to `address` uses.
    pub fn target_of(&self, from: Loc, address: u32) -> Target {
        if let Some(&bank) = self.references.get(&from) {
            return match self.memory.resolve(Some(bank), address) {
                Resolved::Rom(loc) if loc.bank == bank => Target::Rom(loc),
                _ => Target::Ambiguous(address),
            };
        }
        match self.memory.resolve(Some(from.bank), address) {
            Resolved::Rom(loc) => Target::Rom(loc),
            Resolved::Ambiguous(_) => Target::Ambiguous(address),
            Resolved::Outside => Target::Outside(address),
        }
    }

    /// Like [`Self::target_of`], reporting references it cannot resolve.
    fn resolve_from(&mut self, from: Loc, address: u32) -> Target {
        let target = self.target_of(from, address);
        if let Target::Ambiguous(_) = target {
            let at = self.memory.address(from);
            let message = match self.references.get(&from) {
                Some(bank) => format!(
                    "references gives bank {bank} for ${address:04X}, which that bank does not hold"
                ),
                None => format!(
                    "${address:04X} is held by banks {}; add a references entry with target_bank",
                    join(&self.memory.holders(address))
                ),
            };
            self.issue(IssueKind::UnresolvedBank, Some(from), at, message);
        }
        target
    }

    fn trace_all(&mut self, cancel: &AtomicBool) -> Result<(), Vec<String>> {
        self.seed_vectors();
        // Checked after the vectors, which are a pointer table too.
        let mut inside: Vec<Loc> = self
            .operands
            .keys()
            .filter(|l| self.in_word(**l))
            .copied()
            .collect();
        if !inside.is_empty() {
            inside.sort();
            return Err(inside
                .into_iter()
                .map(|loc| {
                    format!(
                        "operands ${:04X}: the second byte of a word; give the expression at the word",
                        self.memory.address(loc)
                    )
                })
                .collect());
        }
        self.seed_tables();
        let code_regions: Vec<Region> = self
            .regions
            .iter()
            .flatten()
            .filter(|r| r.kind == RegionType::Code)
            .cloned()
            .collect();
        for region in &code_regions {
            self.work.push_back((
                Loc {
                    bank: region.bank,
                    offset: region.start,
                },
                None,
            ));
        }
        self.drain(cancel)?;
        // Code regions are decoded throughout, including parts no flow reaches.
        for region in &code_regions {
            let mut offset = region.start;
            while offset < region.end {
                let loc = Loc {
                    bank: region.bank,
                    offset,
                };
                offset += match self.class[loc.bank][offset] {
                    Class::Insn => self.insns[&loc].len,
                    Class::Unknown => match self.decode_at(loc, None) {
                        Some(insn) => insn.len,
                        None => 1,
                    },
                    _ => 1,
                };
            }
        }
        self.drain(cancel)
    }

    fn drain(&mut self, cancel: &AtomicBool) -> Result<(), Vec<String>> {
        while let Some((loc, from)) = self.work.pop_front() {
            if cancel.load(Ordering::Relaxed) {
                return Err(vec!["Disassembly cancelled".into()]);
            }
            self.trace(loc, from);
        }
        Ok(())
    }

    /// Interrupt vectors of the banks that hold $FFFA-$FFFF.
    fn seed_vectors(&mut self) {
        const NAMES: [&str; 3] = ["NMI", "RESET", "IRQ"];
        let banks: Vec<usize> = self
            .memory
            .prg_banks()
            .filter(|b| b.holds(VECTORS) && b.holds(VECTORS + 5))
            .map(|b| b.index)
            .collect();
        for bank in banks {
            let start = (VECTORS - self.memory.banks[bank].org) as usize;
            // Vectors no region describes form a pointer table (seed_tables
            // traces them); the others are traced here.
            let covered: Vec<bool> = (0..3)
                .map(|i| {
                    let entry = start + 2 * i;
                    self.regions[bank]
                        .iter()
                        .any(|r| r.start < entry + 2 && entry < r.end)
                })
                .collect();
            let mut i = 0;
            while i < 3 {
                if covered[i] {
                    i += 1;
                    continue;
                }
                let first = i;
                while i < 3 && !covered[i] {
                    i += 1;
                }
                let (begin, end) = (start + 2 * first, start + 2 * i);
                self.regions[bank].push(Region {
                    bank,
                    start: begin,
                    end,
                    kind: RegionType::Pointers,
                    pair: None,
                    code_targets: true,
                    adjust: 0,
                    columns: None,
                    comment: None,
                });
                self.class[bank][begin..end].fill(Class::Data);
            }
            self.regions[bank].sort_by_key(|r| r.start);
            for (i, name) in NAMES.into_iter().enumerate() {
                let from = Loc {
                    bank,
                    offset: start + 2 * i,
                };
                let address = u32::from(u16::from_le_bytes([
                    self.memory.byte(from),
                    self.memory.byte(from.at(from.offset + 1)),
                ]));
                let target = self.resolve_from(from, address);
                if let Target::Rom(loc) = target {
                    self.vectors.entry(loc).or_insert(name);
                    if covered[i] {
                        self.work.push_back((loc, Some(from)));
                        self.want(loc, Want::Jump);
                    }
                }
                if covered[i] {
                    self.refs.push(Ref {
                        from,
                        kind: XrefKind::Pointer,
                        target,
                    });
                }
            }
        }
    }

    /// Pointer and low/high byte tables: their targets are labels, or code
    /// entry points for jump tables.
    fn seed_tables(&mut self) {
        let tables: Vec<Region> = self
            .regions
            .iter()
            .flatten()
            .filter(|r| matches!(r.kind, RegionType::Pointers | RegionType::LowBytes))
            .cloned()
            .collect();
        for table in tables {
            for (from, address) in self.table_entries(&table) {
                let before = self.issues.len();
                let target = self.resolve_from(from, address);
                // The high byte of a split table entry is part of it too.
                if let (RegionType::LowBytes, Some(high)) = (table.kind, table.pair) {
                    let offset = high + from.offset - table.start;
                    for issue in &mut self.issues[before..] {
                        issue.spans.push((offset, 1));
                    }
                }
                if let Target::Rom(loc) = target {
                    if table.code_targets {
                        self.work.push_back((loc, Some(from)));
                        self.want(loc, Want::Jump);
                    } else {
                        self.want(loc, Want::Data);
                    }
                }
                self.refs.push(Ref {
                    from,
                    kind: XrefKind::Pointer,
                    target,
                });
            }
        }
    }

    /// The (element location, target address) pairs of a pointer or low byte table.
    pub fn table_entries(&self, table: &Region) -> Vec<(Loc, u32)> {
        let loc = |offset| Loc {
            bank: table.bank,
            offset,
        };
        let value =
            |stored: u32| (stored as i64 - i64::from(table.adjust)).rem_euclid(0x10000) as u32;
        match table.kind {
            RegionType::Pointers => (table.start..table.end.saturating_sub(1))
                .step_by(2)
                .map(|o| {
                    let stored = u16::from_le_bytes([
                        self.memory.byte(loc(o)),
                        self.memory.byte(loc(o + 1)),
                    ]);
                    (loc(o), value(u32::from(stored)))
                })
                .collect(),
            RegionType::LowBytes => {
                let high = table.pair.unwrap_or(table.start);
                (0..table.end - table.start)
                    .map(|i| {
                        let stored = u16::from_le_bytes([
                            self.memory.byte(loc(table.start + i)),
                            self.memory.byte(loc(high + i)),
                        ]);
                        (loc(table.start + i), value(u32::from(stored)))
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// Follows code from `loc` until it stops or reaches known bytes.
    fn trace(&mut self, mut loc: Loc, mut from: Option<Loc>) {
        loop {
            match self.class[loc.bank][loc.offset] {
                Class::Insn => return,
                Class::Unknown => {}
                Class::Operand => {
                    let address = self.memory.address(loc);
                    let source = from.map_or(String::new(), |f| {
                        format!(" from ${:04X}", self.memory.address(f))
                    });
                    self.issue(
                        IssueKind::Overlap,
                        Some(loc),
                        address,
                        format!("Code{source} enters the middle of an instruction at ${address:04X} (a skip trick, or a wrong entry)"),
                    );
                    return;
                }
                Class::Data => {
                    let address = self.memory.address(loc);
                    let source = from.map_or(String::new(), |f| {
                        format!(" from ${:04X}", self.memory.address(f))
                    });
                    self.issue(
                        IssueKind::CodeDataConflict,
                        Some(loc),
                        address,
                        format!(
                            "Code{source} reaches ${address:04X}, which a region describes as data"
                        ),
                    );
                    return;
                }
            }
            let Some(insn) = self.decode_at(loc, from) else {
                return;
            };
            let mut next = loc.offset + insn.len;
            match (insn.mnemonic, insn.mode) {
                (Mnemonic::Jmp, _) | (Mnemonic::Rts | Mnemonic::Rti | Mnemonic::Brk, _) => return,
                (Mnemonic::Jsr, _) => {
                    let routine = match self.flow_target(loc) {
                        Some(target) => self.entries.get(&target).copied().unwrap_or_default(),
                        None => Routine::default(),
                    };
                    if routine.no_return {
                        return;
                    }
                    if routine.inline_bytes > 0
                        && !self.inline_data(loc, next, routine.inline_bytes)
                    {
                        return;
                    }
                    next += routine.inline_bytes;
                }
                _ => {}
            }
            let bank = &self.memory.banks[loc.bank];
            if next >= bank.end {
                // Code continues into the bank that holds the next address.
                let address = bank.org + next as u32;
                match self.memory.resolve(Some(loc.bank), address) {
                    Resolved::Rom(n) if n.bank != loc.bank => {
                        from = Some(loc);
                        loc = n;
                        continue;
                    }
                    _ => {
                        self.issue(
                            IssueKind::FlowLeavesRom,
                            Some(loc),
                            address,
                            format!(
                                "Code runs past the end of bank {} into ${address:04X}",
                                loc.bank
                            ),
                        );
                        return;
                    }
                }
            }
            from = Some(loc);
            loc = loc.at(next);
        }
    }

    /// The ROM target of the flow instruction at `loc`, if resolved.
    fn flow_target(&self, loc: Loc) -> Option<Loc> {
        self.flow.get(&loc).copied()
    }

    /// Marks the inline arguments after a JSR as data.
    fn inline_data(&mut self, call: Loc, start: usize, count: usize) -> bool {
        let bank = &self.memory.banks[call.bank];
        let end = start + count;
        if end > bank.end
            || self.class[call.bank][start..end]
                .iter()
                .any(|c| matches!(c, Class::Insn | Class::Operand))
        {
            let address = self.memory.address(call);
            self.issue(
                IssueKind::CodeDataConflict,
                Some(call),
                address,
                format!("The {count} inline bytes after the JSR at ${address:04X} overlap other code or data"),
            );
            return false;
        }
        // Bytes a region already describes keep it; the others become bytes.
        let mut offset = start;
        while offset < end {
            if self.class[call.bank][offset] != Class::Unknown {
                offset += 1;
                continue;
            }
            let piece = offset;
            while offset < end && self.class[call.bank][offset] == Class::Unknown {
                offset += 1;
            }
            self.class[call.bank][piece..offset].fill(Class::Data);
            self.regions[call.bank].push(Region {
                bank: call.bank,
                start: piece,
                end: offset,
                kind: RegionType::Bytes,
                pair: None,
                code_targets: false,
                adjust: 0,
                columns: None,
                comment: None,
            });
        }
        self.regions[call.bank].sort_by_key(|r| r.start);
        true
    }

    /// Decodes and records the instruction at `loc`; `None` if no valid one starts there.
    fn decode_at(&mut self, loc: Loc, from: Option<Loc>) -> Option<Insn> {
        let address = self.memory.address(loc);
        let code = self.memory.byte(loc);
        let end = self.memory.banks[loc.bank].end;
        let decoded = self.decode[usize::from(code)].filter(|_| !inspect::is_cmos_only(code));
        let source = from.map_or(String::new(), |f| {
            format!(" (from ${:04X})", self.memory.address(f))
        });
        let Some((mnemonic, mode)) = decoded else {
            self.issue(
                IssueKind::InvalidOpcode,
                Some(loc),
                address,
                format!("${code:02X} at ${address:04X}{source} is not an NMOS 6502 instruction; the flow may be wrong, or this is data"),
            );
            return None;
        };
        let len = inspect::operand_size(mode) + 1;
        if loc.offset + len > end
            || self.class[loc.bank][loc.offset + 1..loc.offset + len]
                .iter()
                .any(|c| *c != Class::Unknown)
        {
            self.issue(
                IssueKind::Overlap,
                Some(loc),
                address,
                format!("The instruction at ${address:04X}{source} would overlap other code, data or the end of the bank"),
            );
            if let Some(issue) = self.issues.last_mut() {
                issue.spans = vec![(loc.offset, len.min(end - loc.offset))];
            }
            return None;
        }
        let byte = |i: usize| u32::from(self.memory.byte(loc.at(loc.offset + i)));
        let operand = match len {
            2 => byte(1),
            3 => byte(1) | byte(2) << 8,
            _ => 0,
        };
        let value = if mode == Mode::Rel {
            (address + 2).wrapping_add(operand as u8 as i8 as u32) & 0xffff
        } else {
            operand
        };
        let insn = Insn {
            mnemonic,
            mode,
            len,
            value,
        };
        self.class[loc.bank][loc.offset] = Class::Insn;
        self.class[loc.bank][loc.offset + 1..loc.offset + len].fill(Class::Operand);
        self.insns.insert(loc, insn);
        let before = self.issues.len();
        self.reference(loc, insn);
        for issue in &mut self.issues[before..] {
            if issue.spans == [(loc.offset, 1)] {
                issue.spans = vec![(loc.offset, len)];
            }
        }
        Some(insn)
    }

    /// Records what the instruction at `loc` refers to and queues code targets.
    fn reference(&mut self, loc: Loc, insn: Insn) {
        let address = self.memory.address(loc);
        let kind = match (insn.mnemonic, insn.mode) {
            (_, Mode::Imp | Mode::Acc | Mode::Imm) => return,
            (Mnemonic::Jsr, _) => XrefKind::Call,
            (Mnemonic::Jmp, Mode::Abs) => XrefKind::Jump,
            (_, Mode::Rel) => XrefKind::Branch,
            (Mnemonic::Jmp, _) => {
                self.issue(
                    IssueKind::IndirectJump,
                    Some(loc),
                    address,
                    format!(
                        "Indirect JMP [${:04X}] at ${address:04X}; describe its targets with a pointers region (code_targets) or entry_points",
                        insn.value
                    ),
                );
                XrefKind::Read
            }
            (
                Mnemonic::Sta
                | Mnemonic::Stx
                | Mnemonic::Sty
                | Mnemonic::Inc
                | Mnemonic::Dec
                | Mnemonic::Asl
                | Mnemonic::Lsr
                | Mnemonic::Rol
                | Mnemonic::Ror,
                _,
            ) => XrefKind::Write,
            _ => XrefKind::Read,
        };
        let zero_page = matches!(
            insn.mode,
            Mode::Zp | Mode::Zpx | Mode::Zpy | Mode::Zi | Mode::Zix | Mode::Ziy
        );
        let target = if zero_page {
            match self.memory.resolve(Some(loc.bank), insn.value) {
                Resolved::Rom(t) => Target::Rom(t),
                _ => Target::Outside(insn.value),
            }
        } else {
            self.resolve_from(loc, insn.value)
        };
        match (&target, kind) {
            (Target::Rom(t), XrefKind::Call) => {
                self.flow.insert(loc, *t);
                self.work.push_back((*t, Some(loc)));
                self.want(*t, Want::Call);
            }
            (Target::Rom(t), XrefKind::Jump | XrefKind::Branch) => {
                self.flow.insert(loc, *t);
                self.work.push_back((*t, Some(loc)));
                self.want(*t, Want::Jump);
            }
            (Target::Rom(t), XrefKind::Read) => self.want(*t, Want::Data),
            // Writes to $8000-$FFFF go to the mapper, whether or not a bank holds the address.
            (_, XrefKind::Write) if !zero_page && insn.value >= 0x8000 => {
                self.issue(
                    IssueKind::MapperWrite,
                    Some(loc),
                    address,
                    format!(
                        "Write to ROM address ${:04X} at ${address:04X}: a mapper register (bank switching?)",
                        insn.value
                    ),
                );
            }
            (Target::Outside(value), XrefKind::Call | XrefKind::Jump) => {
                self.issue(
                    IssueKind::OutsideRom,
                    Some(loc),
                    address,
                    format!("Code at ${value:04X}, outside the ROM, is reached from ${address:04X}; name it with a symbol"),
                );
            }
            _ => {}
        }
        self.refs.push(Ref {
            from: loc,
            kind,
            target,
        });
    }

    /// Gives every wanted address a name: annotations first, then vectors,
    /// then automatic names.
    fn name_labels(&mut self) -> Result<(), Vec<String>> {
        // Code targets inside an instruction are written as `label+n`, so the
        // instruction needs a label.
        let inside: Vec<Loc> = self
            .wanted
            .keys()
            .chain(self.labels.keys())
            .filter(|l| self.class[l.bank][l.offset] == Class::Operand || self.in_word(**l))
            .copied()
            .collect();
        for loc in inside {
            if let Some(start) = self.containing(loc) {
                let want = if self.in_word(loc) {
                    Want::Data
                } else {
                    Want::Jump
                };
                self.want(start, want);
            }
        }
        let user: HashSet<String> = self
            .labels
            .values()
            .flatten()
            .map(|l| l.name.clone())
            .chain(
                self.symbols
                    .iter()
                    .filter(|s| s.user)
                    .map(|s| s.name.clone()),
            )
            .collect();
        let mut taken = user.clone();
        let mut errors = Vec::new();
        let vectors: Vec<(Loc, &str)> = self.vectors.iter().map(|(l, n)| (*l, *n)).collect();
        for (loc, name) in vectors {
            if !self.labels.contains_key(&loc) && taken.insert(name.to_owned()) {
                self.add_label(loc, name, None);
            }
        }
        let wanted: Vec<(Loc, Want)> = self.wanted.iter().map(|(l, w)| (*l, *w)).collect();
        for (loc, want) in wanted {
            if self.labels.contains_key(&loc)
                || self.class[loc.bank][loc.offset] == Class::Operand
                || self.in_word(loc)
            {
                continue;
            }
            let address = self.memory.address(loc);
            let prefix = match want {
                Want::Call => "sub",
                Want::Jump => "L",
                Want::Data => "D",
            };
            let name = if self.memory.holders(address).len() > 1 {
                format!("{prefix}_{:02X}_{address:04X}", loc.bank)
            } else {
                format!("{prefix}_{address:04X}")
            };
            if user.contains(&name) {
                errors.push(format!(
                    "Name '{name}' is also the automatic label of ${address:04X} in bank {}; choose another name",
                    loc.bank
                ));
            }
            taken.insert(name.clone());
            self.add_label(loc, &name, None);
        }
        if self.options.auto_ram_labels {
            let ram: BTreeSet<u32> = self
                .refs
                .iter()
                .filter_map(|r| match r.target {
                    Target::Outside(v) if v < 0x800 => Some(v),
                    _ => None,
                })
                .collect();
            for value in ram {
                if self
                    .symbols
                    .iter()
                    .any(|s| value >= s.value && value < s.value + s.size)
                {
                    continue;
                }
                let name = if value < 0x100 {
                    format!("zp_{value:02X}")
                } else {
                    format!("ram_{value:04X}")
                };
                if user.contains(&name) {
                    errors.push(format!(
                        "Name '{name}' is also an automatic RAM name; choose another name"
                    ));
                }
                self.symbols.push(SymbolEntry {
                    name,
                    value,
                    size: 1,
                    comment: None,
                    user: false,
                });
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// The instruction whose operand holds `loc`.
    pub fn containing(&self, loc: Loc) -> Option<Loc> {
        if self.in_word(loc) {
            return Some(loc.at(loc.offset - 1));
        }
        (loc.offset.saturating_sub(2)..loc.offset)
            .rev()
            .map(|o| loc.at(o))
            .find(|l| {
                self.insns
                    .get(l)
                    .is_some_and(|i| l.offset + i.len > loc.offset)
            })
    }

    /// Whether `loc` is the second byte of a word in a words or pointers region.
    pub fn in_word(&self, loc: Loc) -> bool {
        self.region_at(loc).is_some_and(|r| {
            matches!(r.kind, RegionType::Words | RegionType::Pointers)
                && (loc.offset - r.start) % 2 == 1
                && loc.offset < r.end - (r.end - r.start) % 2
        })
    }

    /// The primary name of a ROM location, or `label+n` inside an instruction.
    pub fn rom_name(&self, loc: Loc) -> Option<String> {
        // Labels inside an instruction are defined relative to it (`name = base+n`).
        if let Some(label) = self.labels.get(&loc).and_then(|l| l.first()) {
            return Some(label.name.clone());
        }
        let start = self.containing(loc)?;
        let label = self.labels.get(&start)?.first()?;
        Some(format!("{}+{}", label.name, loc.offset - start.offset))
    }

    /// The name of a value outside the ROM, from the symbols.
    pub fn symbol_name(&self, value: u32) -> Option<String> {
        self.symbols
            .iter()
            .find(|s| s.value == value)
            .or_else(|| {
                self.symbols
                    .iter()
                    .find(|s| value > s.value && value < s.value + s.size)
            })
            .map(|s| {
                if s.value == value {
                    s.name.clone()
                } else {
                    format!("{}+{}", s.name, value - s.value)
                }
            })
    }

    /// The code of each routine: everything reached from its entry without
    /// following calls, up to the next routine.
    fn find_routines(&mut self) {
        let heads: BTreeSet<Loc> = self
            .wanted
            .iter()
            .filter(|(_, w)| **w == Want::Call)
            .map(|(l, _)| *l)
            .chain(self.vectors.keys().copied())
            .chain(self.entries.keys().copied())
            .filter(|l| self.insns.contains_key(l))
            .collect();
        let mut targets: HashMap<Loc, Vec<Loc>> = HashMap::new();
        for r in &self.refs {
            if let (XrefKind::Jump | XrefKind::Branch, Target::Rom(t)) = (r.kind, &r.target) {
                targets.entry(r.from).or_default().push(*t);
            }
        }
        for &head in &heads {
            let mut body = Vec::new();
            let mut seen = HashSet::new();
            let mut stack = vec![head];
            while let Some(loc) = stack.pop() {
                if (loc != head && heads.contains(&loc)) || !seen.insert(loc) {
                    continue;
                }
                if body.len() >= MAX_ROUTINE {
                    self.truncated.insert(head);
                    break;
                }
                let Some(insn) = self.insns.get(&loc) else {
                    continue;
                };
                body.push(loc);
                stack.extend(targets.get(&loc).into_iter().flatten().copied());
                let stops = matches!(
                    insn.mnemonic,
                    Mnemonic::Jmp | Mnemonic::Rts | Mnemonic::Rti | Mnemonic::Brk
                );
                if !stops {
                    let mut next = loc.offset + insn.len;
                    if insn.mnemonic == Mnemonic::Jsr {
                        let routine = self
                            .flow_target(loc)
                            .and_then(|t| self.entries.get(&t).copied())
                            .unwrap_or_default();
                        if routine.no_return {
                            continue;
                        }
                        next += routine.inline_bytes;
                    }
                    let bank = &self.memory.banks[loc.bank];
                    if next < bank.end {
                        stack.push(loc.at(next));
                    } else if let Resolved::Rom(n) =
                        self.memory.resolve(Some(loc.bank), bank.org + next as u32)
                    {
                        // As traced, code continues into the bank holding the next address.
                        stack.push(n);
                    }
                }
            }
            body.sort();
            for loc in &body {
                self.routine_index.entry(*loc).or_default().push(head);
            }
            self.routines.insert(head, body);
        }
    }

    /// Unclassified ranges, except runs of one value (padding).
    fn report_unknown(&mut self) {
        let mut found = Vec::new();
        for bank in self.memory.prg_banks() {
            let unknown = |o: usize| self.class[bank.index][o] == Class::Unknown;
            // Length of the run of one value at `o` if it is padding.
            let padding = |o: usize| {
                let run = (o..bank.end)
                    .take_while(|p| unknown(*p) && bank.data[*p] == bank.data[o])
                    .count();
                (run >= FILL_RUN).then_some(run)
            };
            let mut offset = bank.start;
            while offset < bank.end {
                if !unknown(offset) {
                    offset += 1;
                    continue;
                }
                if let Some(run) = padding(offset) {
                    offset += run;
                    continue;
                }
                let start = offset;
                while offset < bank.end && unknown(offset) && padding(offset).is_none() {
                    offset += 1;
                }
                found.push((
                    Loc {
                        bank: bank.index,
                        offset: start,
                    },
                    bank.org,
                    offset - start,
                ));
            }
        }
        for (loc, org, length) in found {
            let first = org + loc.offset as u32;
            self.issue(
                IssueKind::UnknownRange,
                Some(loc),
                first,
                format!(
                    "${first:04X}-${:04X} ({length} bytes) is not reached by code nor described by a region",
                    first + length as u32 - 1
                ),
            );
            if let Some(issue) = self.issues.last_mut() {
                issue.spans = vec![(loc.offset, length)];
            }
        }
    }

    /// The region holding `loc`.
    pub fn region_at(&self, loc: Loc) -> Option<&Region> {
        let regions = &self.regions[loc.bank];
        let index = regions.partition_point(|r| r.start <= loc.offset);
        index
            .checked_sub(1)
            .map(|i| &regions[i])
            .filter(|r| loc.offset < r.end)
    }

    /// The routine containing the instruction at `loc`.
    pub fn routine_of(&self, loc: Loc) -> Option<Loc> {
        self.routines_of(loc).first().copied()
    }

    /// Every routine whose code includes the instruction at `loc`.
    pub fn routines_of(&self, loc: Loc) -> &[Loc] {
        self.routine_index.get(&loc).map_or(&[], Vec::as_slice)
    }
}

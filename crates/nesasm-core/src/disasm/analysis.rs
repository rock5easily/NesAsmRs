//! The ROM as banks of CPU addresses, code-flow tracing from the entry
//! points, references and names.

use super::config::{
    self, Address, CommentPosition, DisasmConfig, JOY2, NES_REGISTERS, RegionType,
};
use super::flow::{self, State, Summaries, Summary};
use super::{Issue, IssueKind, RomFormat, XrefKind};
use crate::inspect::{self, HEADER_SIZE, PRG_UNIT, Rom, VECTORS};
use crate::opcode::{Mnemonic, Mode};
use crate::state::{BANK_SIZE, MAX_BANKS};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Instructions followed per routine for the routine report.
pub(crate) const MAX_ROUTINE: usize = 16384;

/// Runs of one value at least this long are padding (`.ds`), not unknown data.
pub(crate) const FILL_RUN: usize = 16;

/// Times the code is traced again with what is known where paths join.
const MAX_ROUNDS: usize = 16;

/// Entries of a jump table after a JSR that are followed.
const MAX_TABLE: usize = 128;

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
    /// With `options.mapper4_fixed_prg_16k`: the second-to-last PRG bank,
    /// which continues into the last one ($C000-$FFFF as one range).
    pub pair: Option<usize>,
    /// The iNES mapper number.
    pub mapper: Option<u8>,
    pub prg_count: usize,
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
            for (i, layout) in config.banks.iter().enumerate() {
                let org = layout.org.0;
                if org % BANK_SIZE as u32 != 0 || org > 0xe000 {
                    return Err(format!(
                        "banks[{i}]: Bank {} org {} must be a multiple of $2000 up to $E000",
                        layout.bank, layout.org
                    ));
                }
                match banks.get_mut(layout.bank) {
                    Some(bank) if !bank.chr => bank.org = org,
                    _ => {
                        return Err(format!(
                            "banks[{i}]: Bank {} is not a PRG bank of the ROM",
                            layout.bank
                        ));
                    }
                }
            }
        }
        let pair = if config.options.mapper4_fixed_prg_16k {
            Some(fixed_pair(&rom, &banks, prg_count, config)?)
        } else {
            None
        };
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
            prg_count: banks.iter().filter(|b| !b.chr).count(),
            banks,
            payload_len: payload.len(),
            pair,
            mapper: rom.header.as_ref().map(|h| h.mapper),
        })
    }

    /// The location `n` bytes after `loc`: in its bank, or for the first bank
    /// of the fixed pair, in the next one.
    pub fn step(&self, loc: Loc, n: usize) -> Option<Loc> {
        let bank = &self.banks[loc.bank];
        let offset = loc.offset + n;
        if offset < bank.end {
            return Some(loc.at(offset));
        }
        if self.pair != Some(loc.bank) {
            return None;
        }
        let next = &self.banks[loc.bank + 1];
        let offset = offset - BANK_SIZE;
        (offset < next.end).then_some(Loc {
            bank: next.index,
            offset,
        })
    }

    /// The location `n` bytes before `loc`, as [`Self::step`] goes.
    pub fn back(&self, loc: Loc, n: usize) -> Option<Loc> {
        if let Some(offset) = loc.offset.checked_sub(n) {
            return (offset >= self.banks[loc.bank].start).then_some(loc.at(offset));
        }
        let previous = loc.bank.checked_sub(1)?;
        (self.pair == Some(previous) && n - loc.offset <= BANK_SIZE).then(|| Loc {
            bank: previous,
            offset: BANK_SIZE + loc.offset - n,
        })
    }

    /// The banks seen together from `bank`: the fixed pair, or the bank alone.
    fn window(&self, bank: usize) -> Vec<usize> {
        match self.pair {
            Some(first) if bank == first || bank == first + 1 => vec![first, first + 1],
            _ => vec![bank],
        }
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

    /// Where `address` points, seen from `from` (whose own window wins;
    /// the fixed pair is one window).
    pub fn resolve(&self, from: Option<usize>, address: u32) -> Resolved {
        for bank in from.map(|b| self.window(b)).unwrap_or_default() {
            let bank = &self.banks[bank];
            if bank.holds(address) {
                return Resolved::Rom(Loc {
                    bank: bank.index,
                    offset: (address - bank.org) as usize,
                });
            }
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

    /// The window (0 for $8000, 1 for $A000) a PRG bank is switched into.
    pub fn window_of(&self, bank: usize) -> Option<usize> {
        let b = &self.banks[bank];
        (!b.chr && (b.org == 0x8000 || b.org == 0xa000))
            .then(|| (b.org as usize - 0x8000) / BANK_SIZE)
    }

    /// The PRG bank the mapper selects with `bank` (wrapped to the ROM's
    /// banks).
    pub fn bank_number(&self, bank: u8) -> usize {
        usize::from(bank) % self.prg_count.max(1)
    }

    /// The location of `address` in the PRG bank numbered `bank` by the
    /// mapper (wrapped to the ROM's banks), if that bank holds it.
    pub fn in_bank(&self, bank: u8, address: u32) -> Option<Loc> {
        let bank = self.bank_number(bank);
        let image = &self.banks[bank];
        image.holds(address).then(|| Loc {
            bank,
            offset: (address - image.org) as usize,
        })
    }
}

/// The window (0 for $8000, 1 for $A000) of a switchable address.
pub(crate) fn window_index(address: u32) -> Option<usize> {
    (0x8000..0xc000)
        .contains(&address)
        .then(|| (address as usize - 0x8000) / BANK_SIZE)
}

/// The second-to-last PRG bank, checked for `options.mapper4_fixed_prg_16k`.
fn fixed_pair(
    rom: &Rom,
    banks: &[BankImage],
    prg_count: usize,
    config: &DisasmConfig,
) -> Result<usize, String> {
    const OPTION: &str = "options.mapper4_fixed_prg_16k";
    let Some(header) = &rom.header else {
        return Err(format!(
            "{OPTION} applies only to iNES ROMs of mapper 4 (MMC3)"
        ));
    };
    if header.mapper != 4 {
        return Err(format!(
            "{OPTION} applies only to mapper 4 (MMC3); the ROM uses mapper {}",
            header.mapper
        ));
    }
    if prg_count < 2 {
        return Err(format!(
            "{OPTION} needs at least two 8 KiB PRG banks; the ROM has {prg_count}"
        ));
    }
    let first = prg_count - 2;
    if banks[first + 1].end != BANK_SIZE {
        return Err(format!(
            "{OPTION} needs whole 8 KiB PRG banks; bank {} has {} bytes",
            first + 1,
            banks[first + 1].end
        ));
    }
    for layout in &config.banks {
        let fixed = match layout.bank {
            b if b == first => 0xc000,
            b if b == first + 1 => 0xe000,
            _ => continue,
        };
        if layout.org.0 != fixed {
            return Err(format!(
                "banks gives bank {} org {}, but {OPTION} places it at {}",
                layout.bank,
                layout.org,
                Address(fixed)
            ));
        }
    }
    Ok(first)
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
    /// For pointer and low byte tables: offset of the table of the bank
    /// switched in for each entry.
    pub bank_table: Option<usize>,
    /// The index of the annotation that describes the region.
    pub index: Option<usize>,
}

impl Region {
    /// How errors name the region: by the index of its annotation.
    fn name(&self) -> String {
        self.index
            .map_or_else(|| "regions".to_owned(), |i| format!("regions[{i}]"))
    }

    fn new(bank: usize, start: usize, end: usize, kind: RegionType) -> Self {
        Self {
            bank,
            start,
            end,
            kind,
            pair: None,
            code_targets: false,
            adjust: 0,
            columns: None,
            comment: None,
            bank_table: None,
            index: None,
        }
    }
}

/// A jump table found after the JSRs to a routine that jumps through its
/// return address; it grows an entry at a time.
#[derive(Clone, Debug)]
struct JumpTable {
    call: Loc,
    callee: Loc,
    start: Loc,
    entries: usize,
    /// The banks switched in at the call.
    windows: [Option<u8>; 2],
    /// The first target after the table in its bank: the table ends before.
    bound: Option<usize>,
    /// No more entries.
    done: bool,
}

/// An address written to RAM: the writing instruction, the address and the
/// banks switched in.
type StoredPointer = (Loc, u32, [Option<u8>; 2]);

/// A ROM table a value was read from: its address and index register.
type Source = (u32, char);

/// Values loaded from ROM tables along a path, for advice on jumps through
/// them: the table address and its index register.
#[derive(Clone, Debug, Default)]
struct Sources {
    a: Option<Source>,
    x: Option<Source>,
    y: Option<Source>,
    memory: Vec<(u32, Source)>,
    stack: Vec<Option<Source>>,
}

impl Sources {
    fn step(&mut self, insn: Insn) {
        let table = match insn.mode {
            Mode::Ax if insn.value >= 0x8000 => Some((insn.value, 'X')),
            Mode::Ay if insn.value >= 0x8000 => Some((insn.value, 'Y')),
            _ => None,
        };
        match insn.mnemonic {
            Mnemonic::Lda => self.a = table,
            Mnemonic::Ldx => self.x = table,
            Mnemonic::Ldy => self.y = table,
            Mnemonic::Tax => self.x = self.a,
            Mnemonic::Tay => self.y = self.a,
            Mnemonic::Txa => self.a = self.x,
            Mnemonic::Tya => self.a = self.y,
            Mnemonic::Sta | Mnemonic::Stx | Mnemonic::Sty => {
                if matches!(insn.mode, Mode::Zp | Mode::Abs) {
                    let source = match insn.mnemonic {
                        Mnemonic::Sta => self.a,
                        Mnemonic::Stx => self.x,
                        _ => self.y,
                    };
                    self.memory.retain(|(a, _)| *a != insn.value);
                    if let Some(source) = source {
                        self.memory.push((insn.value, source));
                    }
                }
            }
            Mnemonic::Pha => self.stack.push(self.a),
            Mnemonic::Php => self.stack.push(None),
            Mnemonic::Pla => self.a = self.stack.pop().flatten(),
            Mnemonic::Plp => {
                self.stack.pop();
            }
            Mnemonic::Inx | Mnemonic::Dex | Mnemonic::Tsx => self.x = None,
            Mnemonic::Iny | Mnemonic::Dey => self.y = None,
            Mnemonic::Adc
            | Mnemonic::Sbc
            | Mnemonic::And
            | Mnemonic::Ora
            | Mnemonic::Eor
            | Mnemonic::Asl
            | Mnemonic::Lsr
            | Mnemonic::Rol
            | Mnemonic::Ror => self.a = None,
            _ => {}
        }
    }

    fn at(&self, address: u32) -> Option<Source> {
        self.memory
            .iter()
            .find(|(a, _)| *a == address)
            .map(|(_, s)| *s)
    }
}

/// How to describe the code targets of the low and high bytes read from
/// `low` and `high`, adjusted by `adjust`.
fn table_advice(low: Source, high: Source, adjust: i32) -> Option<String> {
    if low.1 != high.1 {
        return None;
    }
    let adjust = if adjust == 0 {
        String::new()
    } else {
        format!(", \"adjust\": {adjust}")
    };
    Some(if high.0 == low.0 + 1 {
        format!(
            "the address comes from the table at ${:04X} indexed by {}: describe it with a pointers region (\"code_targets\": true{adjust})",
            low.0, low.1
        )
    } else {
        format!(
            "the address comes from the low bytes at ${:04X} and the high bytes at ${:04X} indexed by {}: describe them with a low_bytes region (\"pair\": \"${:04X}\", \"code_targets\": true{adjust}) and a high_bytes region",
            low.0, high.0, low.1, high.0
        )
    })
}

/// How an issue could be settled in the annotations, from which its
/// suggestions are made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fix {
    /// Place `bank` at `org` (a `banks` entry).
    Layout { bank: usize, org: u32 },
    /// The reference at `from` goes to one of `banks` (`references`).
    Reference { from: Loc, banks: Vec<usize> },
    /// The routine switches in the banks held in these RAM addresses.
    SwitchBanks {
        routine: Loc,
        banks: [Option<u16>; 2],
    },
    /// The routine takes its return address (`inline_bytes` or `no_return`).
    ReturnAddress { routine: Loc },
    /// A jump table after a JSR, of `entries` words found (0: none found).
    JumpTable { start: Loc, entries: usize },
    /// The targets of a jump come from a table of words (`low` + 1 is
    /// `high`) or from tables of low and high bytes.
    Table { low: Loc, high: Loc, adjust: i32 },
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
    switch_banks: Option<[Option<u16>; 2]>,
    keeps_banks: bool,
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
    /// Banks of references found by following the bank switches (and
    /// `bank_table`), by referring location.
    inferred: HashMap<Loc, usize>,
    entries: HashMap<Loc, Routine>,
    /// ROM targets of calls, jumps and branches.
    flow: HashMap<Loc, Loc>,
    /// For each JSR traced: the bytes after it that are arguments, or `None`
    /// when the code after it is not followed.
    calls: HashMap<Loc, Option<usize>>,
    wanted: BTreeMap<Loc, Want>,
    /// Code to trace, in the order found, with what is known there.
    work: VecDeque<(Loc, Option<Loc>, State)>,
    decode: [Option<(Mnemonic, Mode)>; 256],
    /// What is known where paths join, from the previous rounds.
    joins: HashMap<Loc, State>,
    /// What was known when each instruction was traced first.
    arrivals: HashMap<Loc, State>,
    /// Joins of paths that knew different values: traced again.
    new_joins: HashMap<Loc, State>,
    summaries: Summaries,
    /// Pointers of indirect JMPs not known along their paths.
    jump_vectors: BTreeSet<u32>,
    /// Zero page pointers that code reads data through.
    data_pointers: HashSet<u32>,
    /// Constant addresses written to RAM, by the low byte's address: the
    /// writing instruction, the address and the banks switched in.
    stored_pointers: BTreeMap<u32, Vec<StoredPointer>>,
    followed_pointers: HashSet<(Loc, u32)>,
    /// Stored pointers looked at (followed or not).
    considered_pointers: HashSet<(Loc, u32)>,
    tables: Vec<JumpTable>,
    /// Issues of indirect JMPs, with their pointer.
    jump_issues: Vec<(usize, u32)>,
    /// Routines called that switch in banks held in RAM on the paths that
    /// can be followed (see `Summary::hint`).
    switch_hints: BTreeMap<Loc, [Option<u16>; 2]>,
    /// Pointers in RAM written from ROM tables, by the low byte's address:
    /// the instruction that completed the pointer, and the low and high
    /// byte tables with their index register.
    table_pointers: BTreeMap<u32, (Loc, Source, Source)>,
}

impl Analysis {
    /// Traces the code until what is known where paths join no longer
    /// changes: a join of paths that knew different values (registers, RAM,
    /// switched banks) is traced again knowing only what they share.
    pub fn run(
        file: &[u8],
        config: &DisasmConfig,
        cancel: &AtomicBool,
    ) -> Result<Self, Vec<String>> {
        let mut joins: HashMap<Loc, State> = HashMap::new();
        let mut summaries = Summaries::default();
        let mut round = 1;
        loop {
            let mut analysis = Self::new(file, config, joins, summaries)?;
            analysis.trace_all(cancel)?;
            if analysis.new_joins.is_empty() || round == MAX_ROUNDS {
                analysis.name_labels()?;
                analysis.find_routines();
                analysis.report_unknown();
                return Ok(analysis);
            }
            joins = std::mem::take(&mut analysis.joins);
            for (loc, state) in analysis.new_joins.drain() {
                let met = match joins.get(&loc) {
                    Some(join) => join.meet(&state),
                    None => state,
                };
                joins.insert(loc, met);
            }
            summaries = std::mem::take(&mut analysis.summaries);
            round += 1;
        }
    }

    fn new(
        file: &[u8],
        config: &DisasmConfig,
        joins: HashMap<Loc, State>,
        summaries: Summaries,
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
            inferred: HashMap::new(),
            entries: HashMap::new(),
            flow: HashMap::new(),
            calls: HashMap::new(),
            wanted: BTreeMap::new(),
            work: VecDeque::new(),
            decode: inspect::decode_table(),
            joins,
            arrivals: HashMap::new(),
            new_joins: HashMap::new(),
            summaries,
            jump_vectors: BTreeSet::new(),
            data_pointers: HashSet::new(),
            stored_pointers: BTreeMap::new(),
            followed_pointers: HashSet::new(),
            considered_pointers: HashSet::new(),
            tables: Vec::new(),
            jump_issues: Vec::new(),
            switch_hints: BTreeMap::new(),
            table_pointers: BTreeMap::new(),
        };
        analysis.apply(config)?;
        analysis.check_header();
        Ok(analysis)
    }

    fn issue(&mut self, kind: IssueKind, loc: Option<Loc>, address: u32, message: String) {
        self.issues.push(Issue {
            kind,
            bank: loc.map(|l| l.bank),
            address: Address(address),
            message,
            suggestions: Vec::new(),
            id: String::new(),
            spans: loc.map(|l| (l.offset, 1)).into_iter().collect(),
            fix: None,
            topic: None,
            via: None,
        });
    }

    /// Records the instruction whose path reached the last issue, for its id.
    fn via(&mut self, from: Option<Loc>) {
        let via = from.map(|f| (f.bank, self.memory.address(f)));
        if let Some(issue) = self.issues.last_mut() {
            issue.via = via;
        }
    }

    /// A header issue: they all name $0000, so `topic` tells them apart.
    fn header_issue(&mut self, topic: &'static str, message: String) {
        self.issue(IssueKind::Header, None, 0, message);
        if let Some(issue) = self.issues.last_mut() {
            issue.topic = Some(topic);
        }
    }

    /// Records how the last issue could be settled.
    fn fix(&mut self, fix: Option<Fix>) {
        if let Some(issue) = self.issues.last_mut() {
            issue.fix = fix;
        }
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
                errors.push(format!("{what}: {e}"));
            } else if let Some(other) = names.insert(name.to_owned(), what.clone()) {
                errors.push(format!("Name '{name}' is used by {other} and {what}"));
            }
        };
        for (i, entry) in config.entry_points.iter().enumerate() {
            let what = format!("entry_points[{i}] {}", entry.address);
            if entry.inline_bytes > BANK_SIZE {
                errors.push(format!("{what}: inline_bytes must be at most {BANK_SIZE}"));
                continue;
            }
            match self.memory.locate(entry.bank, entry.address) {
                Ok(loc) => {
                    if let Some(name) = &entry.name {
                        claim(name, what.clone(), &mut errors);
                        self.add_label(loc, name, entry.comment.clone());
                    } else if let Some(comment) = &entry.comment {
                        self.comment(loc, comment, CommentPosition::Above);
                    }
                    let mut switch_banks = None;
                    if entry.switch_banks.is_some() && entry.keeps_banks {
                        errors.push(format!(
                            "{what}: give switch_banks or keeps_banks, not both"
                        ));
                    }
                    if let Some(banks) = entry.switch_banks {
                        for address in banks.iter().flatten() {
                            if !(address.0 < 0x800 || (0x6000..0x8000).contains(&address.0)) {
                                errors.push(format!(
                                    "{what}: switch_banks {address} is not RAM ($0000-$07FF, $6000-$7FFF)"
                                ));
                            }
                        }
                        switch_banks = Some(banks.map(|b| b.map(|a| a.0 as u16)));
                    }
                    self.entries.insert(
                        loc,
                        Routine {
                            no_return: entry.no_return,
                            inline_bytes: entry.inline_bytes,
                            switch_banks,
                            keeps_banks: entry.keeps_banks,
                        },
                    );
                    self.queue(loc, None, State::default());
                    self.want(loc, Want::Call);
                }
                Err(e) => errors.push(format!("{what}: {e}")),
            }
        }
        for (i, label) in config.labels.iter().enumerate() {
            let what = format!("labels[{i}] {} ({})", label.address, label.name);
            match self.memory.locate(label.bank, label.address) {
                Ok(loc) => {
                    claim(&label.name, what, &mut errors);
                    self.add_label(loc, &label.name, label.comment.clone());
                }
                Err(e) => errors.push(format!("{what}: {e}")),
            }
        }
        for (i, symbol) in config.symbols.iter().enumerate() {
            let what = format!("symbols[{i}] {} ({})", symbol.value, symbol.name);
            claim(&symbol.name, what.clone(), &mut errors);
            if symbol.value.0 > 0xffff {
                errors.push(format!(
                    "{what}: Symbol {} value {} exceeds $FFFF",
                    symbol.name, symbol.value
                ));
            }
            if symbol.size == Some(0) {
                errors.push(format!(
                    "{what}: Symbol {} size must be at least 1",
                    symbol.name
                ));
            }
            if u64::from(symbol.value.0) + u64::from(symbol.size.unwrap_or(1)) > 0x10000 {
                errors.push(format!(
                    "{what}: Symbol {} covers addresses past $FFFF",
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
        for (i, comment) in config.comments.iter().enumerate() {
            match self.memory.locate(comment.bank, comment.address) {
                Ok(loc) => self.comment(loc, &comment.text, comment.position),
                Err(e) => errors.push(format!("comments[{i}] {}: {e}", comment.address)),
            }
        }
        for (i, operand) in config.operands.iter().enumerate() {
            match self.memory.locate(operand.bank, operand.address) {
                Ok(_) if operand.expr.trim().is_empty() || operand.expr.contains(['\n', ';']) => {
                    errors.push(format!(
                        "operands[{i}] {}: the expression must be one line without a comment",
                        operand.address
                    ));
                }
                Ok(loc) => {
                    self.operands.insert(loc, operand.expr.trim().to_owned());
                }
                Err(e) => errors.push(format!("operands[{i}] {}: {e}", operand.address)),
            }
        }
        for (i, reference) in config.references.iter().enumerate() {
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
                    "references[{i}] {}: bank {} is not a PRG bank of the ROM",
                    reference.address, reference.target_bank
                )),
                Err(e) => errors.push(format!("references[{i}] {}: {e}", reference.address)),
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
        for (i, def) in config.regions.iter().enumerate() {
            let what = format!("regions[{i}] {}", def.start);
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
            let bank_table = match (def.kind, def.bank_table) {
                (_, None) => None,
                (RegionType::Pointers | RegionType::LowBytes, Some(table)) => {
                    let count = if def.kind == RegionType::Pointers {
                        (end - loc.offset) / 2
                    } else {
                        end - loc.offset
                    };
                    let count = Some(count.max(1) as u32);
                    match self.memory.span(Some(loc.bank), table, None, count) {
                        Ok((t, _)) => Some(t.offset),
                        Err(e) => {
                            errors.push(format!("{what}: bank_table {table}: {e}"));
                            continue;
                        }
                    }
                }
                (_, Some(_)) => {
                    errors.push(format!(
                        "{what}: bank_table applies only to pointers and low_bytes"
                    ));
                    continue;
                }
            };
            self.regions[loc.bank].push(Region {
                pair,
                code_targets: def.code_targets,
                adjust: def.adjust,
                columns: def.columns,
                comment: def.comment.clone(),
                bank_table,
                index: Some(i),
                ..Region::new(loc.bank, loc.offset, end, def.kind)
            });
        }
        for (bank, regions) in self.regions.iter_mut().enumerate() {
            regions.sort_by_key(|r| r.start);
            for pair in regions.windows(2) {
                if pair[1].start < pair[0].end {
                    let org = self.memory.banks[bank].org;
                    errors.push(format!(
                        "{} and {}: Regions at ${:04X} and ${:04X} in bank {bank} overlap",
                        pair[0].name(),
                        pair[1].name(),
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
                        "{} ${:04X}: pair must be the start of a high_bytes region of the same length",
                        low.name(),
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
                        "{} ${:04X}: no low_bytes region names this high_bytes table as its pair",
                        high.name(),
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
                self.header_issue(
                    "nes2",
                    "Header bytes 7 (low nibble) and 8-15 are not zero (NES 2.0 or extra fields); NESASM writes them as zero".into(),
                );
            }
            if self.memory.trainer {
                self.header_issue(
                    "trainer",
                    "The ROM has a 512-byte trainer, which NESASM cannot produce".into(),
                );
            }
            let declared = usize::from(header[4]) * PRG_UNIT + usize::from(header[5]) * BANK_SIZE;
            if declared != self.memory.payload_len {
                self.header_issue(
                    "size",
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
            self.header_issue(
                "partial_bank",
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
        if let Some(&bank) = self.inferred.get(&from)
            && let Resolved::Rom(loc) = self.memory.resolve(Some(bank), address)
            && loc.bank == bank
        {
            return Target::Rom(loc);
        }
        self.default_target(from, address)
    }

    fn default_target(&self, from: Loc, address: u32) -> Target {
        match self.memory.resolve(Some(from.bank), address) {
            Resolved::Rom(loc) => Target::Rom(loc),
            Resolved::Ambiguous(_) => Target::Ambiguous(address),
            Resolved::Outside => Target::Outside(address),
        }
    }

    /// The location of `address` referred to from `from` when the bank
    /// `bank` is known to be switched in (or to hold the target), if it
    /// holds it; recorded for the source to name it the same way.
    fn infer(&mut self, from: Loc, address: u32, bank: u8) -> Option<Loc> {
        let loc = self.memory.in_bank(bank, address)?;
        if self.default_target(from, address) != Target::Rom(loc) {
            self.inferred.insert(from, loc.bank);
        }
        Some(loc)
    }

    /// Like [`Self::resolve_from`], for an instruction on a path where the
    /// banks at $8000 and $A000 may be known.
    fn resolve_with(&mut self, from: Loc, address: u32, state: &State) -> Target {
        if !self.references.contains_key(&from)
            && let Some(window) = window_index(address)
            && let Some(bank) = state.window(window)
        {
            if let Some(loc) = self.infer(from, address, bank) {
                return Target::Rom(loc);
            }
            // Another bank holding the address is not the one switched in.
            let bank = self.memory.bank_number(bank);
            let at = self.memory.address(from);
            let org = 0x8000 + window as u32 * BANK_SIZE as u32;
            let message = format!(
                "${address:04X} is in bank {bank} here (switched in at ${org:04X}), but the layout places bank {bank} at ${:04X}; add {{\"bank\": {bank}, \"org\": \"${org:04X}\"}} to banks",
                self.memory.banks[bank].org
            );
            self.issue(IssueKind::UnresolvedBank, Some(from), at, message);
            self.fix(Some(Fix::Layout { bank, org }));
            return Target::Ambiguous(address);
        }
        self.resolve_from(from, address)
    }

    /// Queues code at `loc` to trace: it runs with its own bank switched in.
    fn queue(&mut self, loc: Loc, from: Option<Loc>, mut state: State) {
        if let Some(w) = self.memory.window_of(loc.bank) {
            state.set_window(w, loc.bank as u8);
        }
        self.work.push_back((loc, from, state));
    }

    /// Like [`Self::target_of`], reporting references it cannot resolve.
    fn resolve_from(&mut self, from: Loc, address: u32) -> Target {
        let target = self.target_of(from, address);
        if let Target::Ambiguous(_) = target {
            let at = self.memory.address(from);
            let holders = self.memory.holders(address);
            let (message, fix) = match self.references.get(&from) {
                Some(bank) => (
                    format!(
                        "references gives bank {bank} for ${address:04X}, which that bank does not hold"
                    ),
                    None,
                ),
                None => (
                    format!(
                        "${address:04X} is held by banks {}; add a references entry with target_bank",
                        join(&holders)
                    ),
                    Some(Fix::Reference {
                        from,
                        banks: holders,
                    }),
                ),
            };
            self.issue(IssueKind::UnresolvedBank, Some(from), at, message);
            self.fix(fix);
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
            self.queue(
                Loc {
                    bank: region.bank,
                    offset: region.start,
                },
                None,
                State::default(),
            );
        }
        self.drain(cancel)?;
        self.settle(cancel)?;
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
                        Some(insn) => {
                            let state = State::default();
                            self.reference_at(loc, insn, &state);
                            if insn.mnemonic == Mnemonic::Jmp && insn.mode != Mode::Abs {
                                self.indirect_jump(loc, insn, &Sources::default(), &state);
                            }
                            insn.len
                        }
                        None => 1,
                    },
                    _ => 1,
                };
            }
        }
        self.drain(cancel)?;
        self.settle(cancel)?;
        self.report_found();
        Ok(())
    }

    fn drain(&mut self, cancel: &AtomicBool) -> Result<(), Vec<String>> {
        while let Some((loc, from, state)) = self.work.pop_front() {
            if cancel.load(Ordering::Relaxed) {
                return Err(vec!["Disassembly cancelled".into()]);
            }
            self.trace(loc, from, state);
        }
        Ok(())
    }

    /// Follows what the traced code shows only as a whole, until nothing
    /// more is found: addresses stored into the pointers of indirect jumps,
    /// and the entries of jump tables after JSRs, one at a time (the code
    /// they reach may end a table).
    fn settle(&mut self, cancel: &AtomicBool) -> Result<(), Vec<String>> {
        loop {
            let pointers = self.follow_pointers();
            let tables = self.extend_tables();
            if !pointers && !tables {
                return Ok(());
            }
            self.drain(cancel)?;
        }
    }

    /// Code at the constant addresses stored into the pointer of an
    /// indirect JMP (such as thread entries), unless code also reads data
    /// through that pointer.
    fn follow_pointers(&mut self) -> bool {
        let mut found = Vec::new();
        for &vector in &self.jump_vectors {
            // JMP [$xxFF] reads its high byte from $xx00, not $xx00+$100.
            if self.data_pointers.contains(&vector) || vector & 0xff == 0xff {
                continue;
            }
            for &(store, address, windows) in
                self.stored_pointers.get(&vector).into_iter().flatten()
            {
                if !self.considered_pointers.contains(&(store, vector)) {
                    found.push((store, vector, address, windows));
                }
            }
        }
        let any = !found.is_empty();
        for (store, vector, address, _) in found {
            self.considered_pointers.insert((store, vector));
            // The jump comes later, elsewhere: the banks switched in at
            // $8000/$A000 then are not known.
            if window_index(address).is_some() {
                continue;
            }
            let target = self.target_of(store, address);
            if let Target::Rom(loc) = target {
                self.followed_pointers.insert((store, vector));
                self.queue(loc, Some(store), State::default());
                self.want(loc, Want::Jump);
                self.refs.push(Ref {
                    from: store,
                    kind: XrefKind::Pointer,
                    target,
                });
            }
        }
        any
    }

    /// Adds the next entry of each jump table after a JSR that is still
    /// growing. An entry is two unclassified bytes before the first target
    /// that follows the table, holding the address of an instruction in the
    /// window of the call; the first entry that is not ends the table.
    fn extend_tables(&mut self) -> bool {
        let mut any = false;
        for index in 0..self.tables.len() {
            let table = self.tables[index].clone();
            if table.done {
                continue;
            }
            let Some((element, target)) = self.table_entry(&table) else {
                self.tables[index].done = true;
                continue;
            };
            any = true;
            self.class[element.bank][element.offset..element.offset + 2].fill(Class::Data);
            let regions = &mut self.regions[element.bank];
            match regions.iter_mut().find(|r| r.start == table.start.offset) {
                Some(region) => region.end = element.offset + 2,
                None => {
                    regions.push(Region {
                        code_targets: true,
                        ..Region::new(
                            element.bank,
                            element.offset,
                            element.offset + 2,
                            RegionType::Pointers,
                        )
                    });
                    regions.sort_by_key(|r| r.start);
                }
            }
            let mut state = State::default();
            for (w, bank) in table.windows.into_iter().enumerate() {
                if let Some(bank) = bank {
                    state.set_window(w, bank);
                }
            }
            self.queue(target, Some(element), state);
            self.want(target, Want::Jump);
            self.refs.push(Ref {
                from: element,
                kind: XrefKind::Pointer,
                target: Target::Rom(target),
            });
            let t = &mut self.tables[index];
            t.entries += 1;
            if target.bank == t.start.bank && target.offset > t.start.offset {
                t.bound = Some(t.bound.map_or(target.offset, |b| b.min(target.offset)));
            }
        }
        any
    }

    /// The next entry of a jump table and its target, if it is one.
    fn table_entry(&mut self, table: &JumpTable) -> Option<(Loc, Loc)> {
        if table.entries >= MAX_TABLE {
            return None;
        }
        let low = table.start.at(table.start.offset + 2 * table.entries);
        let high = low.at(low.offset + 1);
        let image = &self.memory.banks[low.bank];
        if high.offset >= image.end
            || table.bound.is_some_and(|b| low.offset + 2 > b)
            || [low, high].iter().any(|l| {
                self.class[l.bank][l.offset] != Class::Unknown || self.region_at(*l).is_some()
            })
        {
            return None;
        }
        let address = u32::from(u16::from_le_bytes([
            self.memory.byte(low),
            self.memory.byte(high),
        ]));
        let target = window_index(address)
            .and_then(|w| table.windows[w])
            .and_then(|bank| self.memory.in_bank(bank, address))
            .or_else(|| match self.target_of(table.call, address) {
                Target::Rom(loc) => Some(loc),
                _ => None,
            })?;
        let inside = target.bank == low.bank
            && target.offset >= table.start.offset
            && target.offset < high.offset + 1;
        let decodes = flow::Code::insn(&*self, target).is_some_and(|i| i.mnemonic != Mnemonic::Brk);
        if inside
            || !decodes
            || matches!(
                self.class[target.bank][target.offset],
                Class::Operand | Class::Data
            )
        {
            return None;
        }
        if self.default_target(low, address) != Target::Rom(target) {
            self.inferred.insert(low, target.bank);
        }
        Some((low, target))
    }

    /// Reports the jump tables found after JSRs, to be confirmed, and the
    /// indirect jumps whose pointers were followed where code stored them.
    fn report_found(&mut self) {
        for (routine, banks) in std::mem::take(&mut self.switch_hints) {
            let address = self.memory.address(routine);
            let shown = |b: Option<u16>| b.map_or("null".to_owned(), |a| format!("\"${a:04X}\""));
            let what: Vec<String> = [(banks[0], "$8000"), (banks[1], "$A000")]
                .iter()
                .filter_map(|(b, w)| b.map(|a| format!("the bank in ${a:04X} at {w}")))
                .collect();
            self.issue(
                IssueKind::BankSwitch,
                Some(routine),
                address,
                format!(
                    "The routine at ${address:04X} switches in {} on the paths that can be followed, but others run code whose effects are not known, so the banks after a call to it are not known; if it always does, give it \"switch_banks\": [{}, {}] in entry_points",
                    what.join(" and "),
                    shown(banks[0]),
                    shown(banks[1])
                ),
            );
            self.fix(Some(Fix::SwitchBanks { routine, banks }));
        }
        for (index, vector) in std::mem::take(&mut self.jump_issues) {
            let mut parts = Vec::new();
            let mut stores: Vec<u32> = self
                .followed_pointers
                .iter()
                .filter(|(_, v)| *v == vector)
                .map(|(l, _)| self.memory.address(*l))
                .collect();
            stores.sort_unstable();
            stores.dedup();
            if !stores.is_empty() {
                let list: Vec<String> = stores.iter().map(|a| format!("${a:04X}")).collect();
                parts.push(format!(
                    "the addresses written to ${vector:04X} at {} are followed as code",
                    list.join(", ")
                ));
            }
            // A pointer loaded from ROM tables elsewhere, as for raster
            // interrupt handlers.
            if self.issues[index].message.ends_with("or entry_points")
                && let Some((store, low, high)) = self.table_pointers.get(&vector).copied()
                && let Some(advice) = self.table_advice(store, low, high, 0)
            {
                parts.push(format!("at ${:04X}, {advice}", self.memory.address(store)));
                self.issues[index].fix = self.table_fix(store, low, high, 0);
            } else if !parts.is_empty() {
                parts.push("give entry_points for its other targets".into());
            }
            if !parts.is_empty() {
                let issue = &mut self.issues[index];
                issue.message = format!(
                    "Indirect JMP [${vector:04X}] at {}: {}",
                    issue.address,
                    parts.join("; ")
                );
            }
        }
        for table in std::mem::take(&mut self.tables) {
            let entries = table.entries;
            let call = self.memory.address(table.call);
            let callee = self.memory.address(table.callee);
            let start = self.memory.address(table.start);
            let message = if entries == 0 {
                format!(
                    "JSR ${callee:04X} at ${call:04X} jumps through its return address, but no entry of a table after it was found; describe the table at ${start:04X} with a pointers region (\"code_targets\": true)"
                )
            } else {
                format!(
                    "JSR ${callee:04X} at ${call:04X} jumps through its return address: {entries} {} of a jump table {} found at ${start:04X}-${:04X}; confirm the length with a pointers region (\"code_targets\": true)",
                    if entries == 1 { "entry" } else { "entries" },
                    if entries == 1 { "was" } else { "were" },
                    start + 2 * entries as u32 - 1
                )
            };
            self.issue(IssueKind::JumpTable, Some(table.start), start, message);
            self.fix(Some(Fix::JumpTable {
                start: table.start,
                entries,
            }));
            if let Some(issue) = self.issues.last_mut() {
                issue.spans = vec![(table.start.offset, (2 * entries).max(1))];
            }
        }
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
                    code_targets: true,
                    ..Region::new(bank, begin, end, RegionType::Pointers)
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
                // A pointer table (the automatic one or the annotations')
                // records the reference and its issues in seed_tables.
                // Only when the table reads the same entry: aligned, whole and unadjusted.
                let table = !covered[i]
                    || self.region_at(from).is_some_and(|r| {
                        r.kind == RegionType::Pointers
                            && (from.offset - r.start).is_multiple_of(2)
                            && from.offset + 2 <= r.end
                            && r.adjust == 0
                    });
                let target = if table {
                    self.target_of(from, address)
                } else {
                    let before = self.issues.len();
                    let target = self.resolve_from(from, address);
                    for issue in &mut self.issues[before..] {
                        issue.spans = vec![(from.offset, 2)];
                    }
                    target
                };
                if let Target::Rom(loc) = target {
                    self.vectors.entry(loc).or_insert(name);
                    if covered[i] {
                        self.queue(loc, Some(from), State::default());
                        self.want(loc, Want::Jump);
                    }
                }
                if !table {
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
            for (i, (from, address)) in self.table_entries(&table).into_iter().enumerate() {
                let before = self.issues.len();
                // The bank of the entry, from `bank_table`, if it holds the address.
                let bank = table.bank_table.map(|t| self.memory.byte(from.at(t + i)));
                let target = match bank.and_then(|b| self.infer(from, address, b)) {
                    Some(loc) => Target::Rom(loc),
                    None => self.resolve_from(from, address),
                };
                // Issues concern every byte of the entry: both bytes of a
                // pointer, the high byte of a split entry too.
                for issue in &mut self.issues[before..] {
                    match (table.kind, table.pair) {
                        (RegionType::LowBytes, Some(high)) => {
                            issue.spans.push((high + from.offset - table.start, 1));
                        }
                        _ => issue.spans = vec![(from.offset, 2)],
                    }
                }
                if let Target::Rom(loc) = target {
                    if table.code_targets {
                        // The table's bank is switched in when it is read,
                        // and so is the entry's.
                        let mut state = State::default();
                        for bank in [table.bank, loc.bank] {
                            if let Some(w) = self.memory.window_of(bank) {
                                state.set_window(w, bank as u8);
                            }
                        }
                        self.queue(loc, Some(from), state);
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

    /// Follows code from `loc` until it stops or reaches known bytes, with
    /// what is known on the way (`state`).
    fn trace(&mut self, loc: Loc, from: Option<Loc>, state: State) {
        let mut stored = Vec::new();
        self.trace_path(loc, from, state, &mut stored);
        self.keep_pointers(&mut stored);
    }

    /// Pointers stored on a path that it did not change again before it
    /// jumps, calls or branches: an indirect JMP may go through them.
    fn keep_pointers(&mut self, stored: &mut Vec<(u32, StoredPointer)>) {
        for (low, pointer) in stored.drain(..) {
            let list = self.stored_pointers.entry(low).or_default();
            if !list.contains(&pointer) {
                list.push(pointer);
            }
        }
    }

    fn trace_path(
        &mut self,
        mut loc: Loc,
        mut from: Option<Loc>,
        mut state: State,
        stored: &mut Vec<(u32, StoredPointer)>,
    ) {
        let mut sources = Sources::default();
        // A constant just stored that may be the first byte of a pointer
        // the next store completes.
        let mut pending: Option<u32> = None;
        loop {
            if let Some(join) = self.joins.get(&loc) {
                let met = state.meet(join);
                if met != state {
                    state = met;
                    sources = Sources::default();
                    pending = None;
                }
            }
            // Code in a switched bank runs with that bank switched in. If
            // the code switched another bank in there, it goes on in that
            // bank, which is not followed.
            if let Some(w) = self.memory.window_of(loc.bank) {
                match state.window(w) {
                    Some(bank) if self.memory.bank_number(bank) != loc.bank => return,
                    // The code itself switched an unknown bank in there.
                    None if state.switched[w] => return,
                    _ => state.set_window(w, loc.bank as u8),
                }
            }
            match self.class[loc.bank][loc.offset] {
                Class::Insn => {
                    self.arrive(loc, &state);
                    return;
                }
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
                    self.via(from);
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
                    self.via(from);
                    return;
                }
            }
            let Some(insn) = self.decode_at(loc, from) else {
                return;
            };
            self.arrivals.insert(loc, state.clone());
            self.reference_at(loc, insn, &state);
            let mut advance = insn.len;
            match (insn.mnemonic, insn.mode) {
                (Mnemonic::Jmp, mode) => {
                    // A routine jumped to returns to a pushed address when a
                    // path of it that can be followed returns.
                    let target = if mode == Mode::Abs {
                        self.flow_target(loc)
                    } else {
                        state.jump_word(insn.value).and_then(|a| {
                            match self.target_of(loc, u32::from(a)) {
                                Target::Rom(t) => Some(t),
                                _ => None,
                            }
                        })
                    };
                    let returns = target.is_some_and(|t| {
                        let summary = self.summary(t, &state);
                        summary.returns_known && !summary.takes_return
                    });
                    if returns {
                        self.pushed_return(loc, &state);
                    }
                    if mode != Mode::Abs {
                        match state.jump_word(insn.value) {
                            Some(address) => self.jump_to(loc, u32::from(address), &state),
                            None => {
                                self.jump_vectors.insert(insn.value);
                                self.indirect_jump(loc, insn, &sources, &state);
                            }
                        }
                    }
                    return;
                }
                (Mnemonic::Rts, _) => {
                    self.rts_jump(loc, &state, &sources);
                    return;
                }
                (Mnemonic::Rti | Mnemonic::Brk, _) => return,
                (Mnemonic::Jsr, _) => {
                    self.keep_pointers(stored);
                    match self.call(loc, insn, &mut state) {
                        Some(inline) => advance += inline,
                        None => return,
                    }
                    sources = Sources::default();
                    pending = None;
                }
                _ => {
                    if insn.mode == Mode::Rel {
                        self.keep_pointers(stored);
                    }
                    sources.step(insn);
                    if matches!(insn.mnemonic, Mnemonic::Sta | Mnemonic::Stx | Mnemonic::Sty) {
                        self.table_pointer(loc, insn.value, &sources);
                    }
                    let at = state.known_windows();
                    let mmc3 = flow::Code::mmc3(&*self);
                    let effect =
                        state.step(insn, |a| flow::Code::rom_byte(&*self, loc, a, at), mmc3);
                    if let Some(address) = effect.stored {
                        // Two stores in a row to the bytes of a pointer set
                        // it; the second does not start another pair.
                        let address = u32::from(address);
                        match pending {
                            Some(first) if first.abs_diff(address) == 1 => {
                                let low = first.min(address);
                                if let Some(word) = state.word(low).map(u32::from)
                                    && word >= 0x8000
                                {
                                    stored.push((low, (loc, word, state.known_windows())));
                                }
                                pending = None;
                            }
                            _ => pending = Some(address),
                        }
                    } else if matches!(
                        insn.mnemonic,
                        Mnemonic::Sta
                            | Mnemonic::Stx
                            | Mnemonic::Sty
                            | Mnemonic::Inc
                            | Mnemonic::Dec
                            | Mnemonic::Asl
                            | Mnemonic::Lsr
                            | Mnemonic::Rol
                            | Mnemonic::Ror
                    ) && insn.mode != Mode::Acc
                    {
                        pending = None;
                    }
                    // A pointer written again is not the one stored.
                    stored.retain(|(low, (_, word, _))| {
                        state.word(*low).map(u32::from) == Some(*word)
                    });
                }
            }
            let Some(next) = self.next_code(loc, advance) else {
                let address = self.memory.address(loc) + advance as u32;
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
            };
            from = Some(loc);
            loc = next;
        }
    }

    /// Another path reaching an instruction traced before: if it knew less,
    /// the instruction is traced again in the next round, knowing only what
    /// both knew.
    fn arrive(&mut self, loc: Loc, state: &State) {
        let Some(first) = self.arrivals.get(&loc) else {
            return;
        };
        let met = first.meet(state);
        if met != *first {
            let join = match self.new_joins.remove(&loc) {
                Some(join) => join.meet(&met),
                None => met,
            };
            self.new_joins.insert(loc, join);
        }
    }

    /// What a call to `entry` does, with the banks known in `state`.
    fn summary(&mut self, entry: Loc, state: &State) -> Arc<Summary> {
        let mut summaries = std::mem::take(&mut self.summaries);
        let summary = summaries.get(&*self, entry, state.known_windows());
        self.summaries = summaries;
        summary
    }

    /// A JSR: applies what the routine does to `state`. Returns the bytes of
    /// arguments after the JSR, or `None` if the code after it is not
    /// followed: the routine does not return, or takes its return address
    /// (arguments or a jump table after the JSR).
    fn call(&mut self, loc: Loc, insn: Insn, state: &mut State) -> Option<usize> {
        let target = self.flow_target(loc);
        let routine = target
            .and_then(|t| self.entries.get(&t).copied())
            .filter(|r| r.no_return || r.inline_bytes > 0);
        let summary = target.map(|t| self.summary(t, state));
        if let (Some(t), Some(hint)) = (target, summary.as_ref().and_then(|s| s.hint)) {
            self.switch_hints.entry(t).or_insert(hint);
        }
        let inline = match (routine, &summary, target) {
            (Some(r), _, _) if r.no_return => None,
            (Some(r), _, _) => self
                .inline_data(loc, insn.len, r.inline_bytes)
                .then_some(r.inline_bytes),
            (None, Some(s), Some(callee)) if s.dispatch => {
                self.jump_table_after(loc, insn.len, callee, state);
                None
            }
            (None, Some(s), Some(callee)) if s.takes_return => {
                let address = self.memory.address(loc);
                self.issue(
                    IssueKind::ReturnAddress,
                    Some(loc),
                    address,
                    format!(
                        "JSR ${:04X} at ${address:04X}: the routine takes its return address (arguments after the JSR?), so the code after the JSR is not followed; give the routine \"inline_bytes\" or \"no_return\" in entry_points",
                        self.memory.address(callee)
                    ),
                );
                self.fix(Some(Fix::ReturnAddress { routine: callee }));
                None
            }
            _ => Some(0),
        };
        self.calls.insert(loc, inline);
        match &summary {
            Some(s) if s.returns => state.apply(s),
            Some(s) => state.forget_after(s),
            None => state.forget(),
        }
        inline
    }

    /// A jump table after a JSR to a routine that jumps through its return
    /// address, unless a region describes the bytes there.
    fn jump_table_after(&mut self, call: Loc, len: usize, callee: Loc, state: &State) {
        let Some(start) = self.memory.step(call, len) else {
            return;
        };
        if self.region_at(start).is_some() || self.tables.iter().any(|t| t.start == start) {
            return;
        }
        self.tables.push(JumpTable {
            call,
            callee,
            start,
            entries: 0,
            windows: state.known_windows(),
            bound: None,
            done: false,
        });
    }

    /// A jump (JMP) after pushing a constant return address: the code there
    /// runs when the routine jumped to returns.
    fn pushed_return(&mut self, loc: Loc, state: &State) {
        if let Some(word) = state.pushed_word() {
            self.continue_at(loc, u32::from(word.wrapping_add(1)), true);
        }
    }

    /// An RTS after pushing two values: it jumps to the address they make,
    /// plus one.
    fn rts_jump(&mut self, loc: Loc, state: &State, sources: &Sources) {
        if let Some(word) = state.pushed_word() {
            self.continue_at(loc, u32::from(word.wrapping_add(1)), false);
            return;
        }
        if state.stack.len() < 2 {
            return;
        }
        let (advice, fix) = match sources.stack.as_slice() {
            [.., Some(high), Some(low)] => (
                self.table_advice(loc, *low, *high, -1),
                self.table_fix(loc, *low, *high, -1),
            ),
            _ => (None, None),
        };
        let advice = advice.unwrap_or_else(|| {
            "describe its targets with a pointers region (code_targets, \"adjust\": -1) or entry_points".into()
        });
        let address = self.memory.address(loc);
        self.issue(
            IssueKind::IndirectJump,
            Some(loc),
            address,
            format!("RTS at ${address:04X} jumps to an address pushed before it; {advice}"),
        );
        self.fix(fix);
    }

    /// Code at `address`, which `from` returns or jumps to, if it can be an
    /// instruction there (in the bank of `from` when `same_bank`).
    fn continue_at(&mut self, from: Loc, address: u32, same_bank: bool) {
        let Target::Rom(target) = self.target_of(from, address) else {
            return;
        };
        let decodes = flow::Code::insn(&*self, target).is_some_and(|i| i.mnemonic != Mnemonic::Brk);
        if (same_bank && !self.memory.window(from.bank).contains(&target.bank))
            || !decodes
            || matches!(
                self.class[target.bank][target.offset],
                Class::Operand | Class::Data
            )
        {
            return;
        }
        self.queue(target, Some(from), State::default());
        self.want(target, Want::Jump);
        self.refs.push(Ref {
            from,
            kind: XrefKind::Jump,
            target: Target::Rom(target),
        });
    }

    /// An indirect JMP through a pointer known along the path.
    fn jump_to(&mut self, loc: Loc, address: u32, state: &State) {
        let target = self.resolve_with(loc, address, state);
        if let Target::Rom(t) = target {
            self.queue(t, Some(loc), state.clone());
            self.want(t, Want::Jump);
            self.refs.push(Ref {
                from: loc,
                kind: XrefKind::Jump,
                target,
            });
        }
    }

    /// How to describe the targets of a jump through the low and high
    /// bytes read from the tables `low` and `high`, unless a region already
    /// describes them.
    fn table_advice(&self, from: Loc, low: Source, high: Source, adjust: i32) -> Option<String> {
        let described = match self.memory.resolve(Some(from.bank), low.0) {
            Resolved::Rom(l) => self
                .region_at(l)
                .is_some_and(|r| r.code_targets && r.start == l.offset),
            _ => false,
        };
        if described {
            return Some(format!(
                "the address comes from the table at ${:04X} indexed by {}, which a region describes",
                low.0, low.1
            ));
        }
        table_advice(low, high, adjust)
    }

    /// The tables [`Self::table_advice`] describes, unless a region already
    /// describes them: both read with the same index, in one bank.
    fn table_fix(&self, from: Loc, low: Source, high: Source, adjust: i32) -> Option<Fix> {
        if low.1 != high.1 {
            return None;
        }
        let locate = |address: u32| match self.memory.resolve(Some(from.bank), address) {
            Resolved::Rom(l) => Some(l),
            _ => None,
        };
        let (low, high) = (locate(low.0)?, locate(high.0)?);
        let described = self
            .region_at(low)
            .is_some_and(|r| r.code_targets && r.start == low.offset);
        // Bytes read from one table are not an address table.
        (low.bank == high.bank && low != high && !described).then_some(Fix::Table {
            low,
            high,
            adjust,
        })
    }

    /// An indirect JMP whose targets are not known: how to describe them.
    fn indirect_jump(&mut self, loc: Loc, insn: Insn, sources: &Sources, state: &State) {
        let address = self.memory.address(loc);
        let vector = insn.value;
        // A return address pushed before: where the targets return, if they do.
        let back = state.pushed_word().map_or(String::new(), |w| {
            format!(
                " (it pushes the return address ${:04X} first: if the targets return, give it in entry_points)",
                w.wrapping_add(1)
            )
        });
        let (advice, fix) = match (sources.at(vector), sources.at(vector + 1)) {
            (Some(low), Some(high)) => (
                self.table_advice(loc, low, high, 0),
                self.table_fix(loc, low, high, 0),
            ),
            _ => (None, None),
        };
        let advice = advice.unwrap_or_else(|| {
            "describe its targets with a pointers region (code_targets) or entry_points".into()
        });
        self.jump_issues.push((self.issues.len(), vector));
        self.issue(
            IssueKind::IndirectJump,
            Some(loc),
            address,
            format!("Indirect JMP [${vector:04X}] at ${address:04X}; {advice}{back}"),
        );
        self.fix(fix);
        let end = self.memory.banks[loc.bank].end;
        if let Some(issue) = self.issues.last_mut() {
            issue.spans = vec![(loc.offset, insn.len.min(end - loc.offset))];
        }
    }

    /// A byte read from a ROM table written to RAM: with the byte before or
    /// after it, read from another table with the same index, the address
    /// an indirect JMP through that RAM may go to.
    fn table_pointer(&mut self, loc: Loc, address: u32, sources: &Sources) {
        for low in [address.wrapping_sub(1), address] {
            if let (Some(l), Some(h)) = (sources.at(low), sources.at(low.wrapping_add(1)))
                && l.1 == h.1
            {
                self.table_pointers.entry(low).or_insert((loc, l, h));
            }
        }
    }

    /// Where code continues `advance` bytes after `loc`: in its bank (or the
    /// fixed pair), or past its end in the bank that holds the next address.
    fn next_code(&self, loc: Loc, advance: usize) -> Option<Loc> {
        if let Some(next) = self.memory.step(loc, advance) {
            return Some(next);
        }
        let address = self.memory.address(loc) + advance as u32;
        match self.memory.resolve(Some(loc.bank), address) {
            Resolved::Rom(n) if n.bank != loc.bank => Some(n),
            _ => None,
        }
    }

    /// The ROM target of the flow instruction at `loc`, if resolved.
    fn flow_target(&self, loc: Loc) -> Option<Loc> {
        self.flow.get(&loc).copied()
    }

    /// Marks the `count` inline arguments `advance` bytes after a JSR as data.
    fn inline_data(&mut self, call: Loc, advance: usize, count: usize) -> bool {
        let bytes: Option<Vec<Loc>> = (advance..advance + count)
            .map(|n| self.memory.step(call, n))
            .collect();
        let Some(bytes) = bytes.filter(|bytes| {
            bytes
                .iter()
                .all(|l| !matches!(self.class[l.bank][l.offset], Class::Insn | Class::Operand))
        }) else {
            let address = self.memory.address(call);
            self.issue(
                IssueKind::CodeDataConflict,
                Some(call),
                address,
                format!("The {count} inline bytes after the JSR at ${address:04X} overlap other code or data"),
            );
            return false;
        };
        // Bytes a region already describes keep it; the others become bytes,
        // a region per run in one bank.
        let mut i = 0;
        while i < bytes.len() {
            let piece = bytes[i];
            let mut end = piece.offset;
            while i < bytes.len()
                && bytes[i] == piece.at(end)
                && self.class[piece.bank][end] == Class::Unknown
            {
                i += 1;
                end += 1;
            }
            if end == piece.offset {
                i += 1;
                continue;
            }
            self.class[piece.bank][piece.offset..end].fill(Class::Data);
            self.regions[piece.bank].push(Region::new(
                piece.bank,
                piece.offset,
                end,
                RegionType::Bytes,
            ));
            self.regions[piece.bank].sort_by_key(|r| r.start);
        }
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
            self.via(from);
            return None;
        };
        let len = inspect::operand_size(mode) + 1;
        // Operand bytes, which continue into the next bank of the fixed pair.
        let operands: Option<Vec<Loc>> = (1..len).map(|n| self.memory.step(loc, n)).collect();
        let Some(operands) = operands.filter(|operands| {
            operands
                .iter()
                .all(|l| self.class[l.bank][l.offset] == Class::Unknown)
        }) else {
            self.issue(
                IssueKind::Overlap,
                Some(loc),
                address,
                format!("The instruction at ${address:04X}{source} would overlap other code, data or the end of the bank"),
            );
            self.via(from);
            if let Some(issue) = self.issues.last_mut() {
                issue.spans = vec![(loc.offset, len.min(end - loc.offset))];
            }
            return None;
        };
        let byte = |i: usize| u32::from(self.memory.byte(operands[i - 1]));
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
        for l in &operands {
            self.class[l.bank][l.offset] = Class::Operand;
        }
        self.insns.insert(loc, insn);
        Some(insn)
    }

    /// Records what the instruction at `loc` refers to, with what `state`
    /// knows, and queues code targets; issues cover the instruction.
    fn reference_at(&mut self, loc: Loc, insn: Insn, state: &State) {
        let end = self.memory.banks[loc.bank].end;
        let before = self.issues.len();
        self.reference(loc, insn, state);
        for issue in &mut self.issues[before..] {
            if issue.spans == [(loc.offset, 1)] {
                issue.spans = vec![(loc.offset, insn.len.min(end - loc.offset))];
            }
        }
    }

    fn reference(&mut self, loc: Loc, insn: Insn, state: &State) {
        let address = self.memory.address(loc);
        if matches!(insn.mode, Mode::Zix | Mode::Ziy | Mode::Zi) {
            self.data_pointers.insert(insn.value);
        }
        let kind = match (insn.mnemonic, insn.mode) {
            (_, Mode::Imp | Mode::Acc | Mode::Imm) => return,
            (Mnemonic::Jsr, _) => XrefKind::Call,
            (Mnemonic::Jmp, Mode::Abs) => XrefKind::Jump,
            (_, Mode::Rel) => XrefKind::Branch,
            // Its pointer; the trace follows or reports where it goes.
            (Mnemonic::Jmp, _) => XrefKind::Read,
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
        // Writes to $8000-$FFFF go to the mapper, whether or not a bank
        // holds the address: no bank needs to be chosen.
        let mapper = kind == XrefKind::Write && !zero_page && insn.value >= 0x8000;
        let target = if zero_page {
            match self.memory.resolve(Some(loc.bank), insn.value) {
                Resolved::Rom(t) => Target::Rom(t),
                _ => Target::Outside(insn.value),
            }
        } else if mapper {
            self.target_of(loc, insn.value)
        } else {
            self.resolve_with(loc, insn.value, state)
        };
        match (&target, kind) {
            _ if mapper => {
                let mut probe = state.clone();
                let what = probe
                    .step(insn, |_| None, flow::Code::mmc3(&*self))
                    .mapper
                    .unwrap_or_else(|| "a mapper register (bank switching?)".into());
                self.issue(
                    IssueKind::MapperWrite,
                    Some(loc),
                    address,
                    format!(
                        "Write to ROM address ${:04X} at ${address:04X}: {what}",
                        insn.value
                    ),
                );
            }
            (Target::Rom(t), XrefKind::Call) => {
                self.flow.insert(loc, *t);
                self.queue(*t, Some(loc), state.call());
                self.want(*t, Want::Call);
            }
            (Target::Rom(t), XrefKind::Jump | XrefKind::Branch) => {
                self.flow.insert(loc, *t);
                self.queue(*t, Some(loc), state.clone());
                self.want(*t, Want::Jump);
            }
            (Target::Rom(t), XrefKind::Read) => self.want(*t, Want::Data),
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
        (1..=2).find_map(|n| {
            let start = self.memory.back(loc, n)?;
            self.insns
                .get(&start)
                .is_some_and(|i| i.len > n)
                .then_some(start)
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
        Some(format!(
            "{}+{}",
            label.name,
            self.memory.address(loc) - self.memory.address(start)
        ))
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
                    let mut advance = insn.len;
                    if insn.mnemonic == Mnemonic::Jsr {
                        // As traced (code regions decode JSRs not traced).
                        let inline = match self.calls.get(&loc) {
                            Some(inline) => *inline,
                            None => {
                                let routine = self
                                    .flow_target(loc)
                                    .and_then(|t| self.entries.get(&t).copied())
                                    .unwrap_or_default();
                                (!routine.no_return).then_some(routine.inline_bytes)
                            }
                        };
                        let Some(inline) = inline else {
                            continue;
                        };
                        advance += inline;
                    }
                    // As traced, code continues into the next bank.
                    stack.extend(self.next_code(loc, advance));
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

impl flow::Code for Analysis {
    fn insn(&self, loc: Loc) -> Option<Insn> {
        let code = self.memory.byte(loc);
        if inspect::is_cmos_only(code) {
            return None;
        }
        let (mnemonic, mode) = self.decode[usize::from(code)]?;
        let len = inspect::operand_size(mode) + 1;
        let operands: Vec<Loc> = (1..len)
            .map(|n| self.memory.step(loc, n))
            .collect::<Option<_>>()?;
        let byte = |i: usize| u32::from(self.memory.byte(operands[i - 1]));
        let operand = match len {
            2 => byte(1),
            3 => byte(1) | byte(2) << 8,
            _ => 0,
        };
        let value = if mode == Mode::Rel {
            (self.memory.address(loc) + 2).wrapping_add(operand as u8 as i8 as u32) & 0xffff
        } else {
            operand
        };
        Some(Insn {
            mnemonic,
            mode,
            len,
            value,
        })
    }

    fn next(&self, loc: Loc, n: usize) -> Option<Loc> {
        self.next_code(loc, n)
    }

    fn target(&self, from: Loc, address: u32, windows: [Option<u8>; 2]) -> Option<Loc> {
        if let Some(&bank) = self.references.get(&from) {
            return match self.memory.resolve(Some(bank), address) {
                Resolved::Rom(loc) if loc.bank == bank => Some(loc),
                _ => None,
            };
        }
        if let Some(window) = window_index(address)
            && let Some(bank) = windows[window]
        {
            return self.memory.in_bank(bank, address);
        }
        match self.memory.resolve(Some(from.bank), address) {
            Resolved::Rom(loc) => Some(loc),
            _ => None,
        }
    }

    fn rom_byte(&self, from: Loc, address: u32, windows: [Option<u8>; 2]) -> Option<u8> {
        if address < 0x8000 {
            return None;
        }
        flow::Code::target(self, from, address, windows).map(|loc| self.memory.byte(loc))
    }

    fn annotated(&self, loc: Loc) -> Option<(bool, usize)> {
        self.entries
            .get(&loc)
            .filter(|r| r.no_return || r.inline_bytes > 0)
            .map(|r| (r.no_return, r.inline_bytes))
    }

    fn switches(&self, loc: Loc) -> Option<[Option<u16>; 2]> {
        self.entries.get(&loc).and_then(|r| r.switch_banks)
    }

    fn keeps(&self, loc: Loc) -> bool {
        self.entries.get(&loc).is_some_and(|r| r.keeps_banks)
    }

    fn window_of(&self, bank: usize) -> Option<usize> {
        self.memory.window_of(bank)
    }

    fn bank_number(&self, bank: u8) -> usize {
        self.memory.bank_number(bank)
    }

    fn mmc3(&self) -> bool {
        self.memory.mapper == Some(4)
    }
}

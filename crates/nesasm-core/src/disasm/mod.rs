//! Disassembly into reassemblable NESASM source.
//!
//! A [`DisasmConfig`] (the annotation file) names the ROM and records what is
//! known about it: labels, symbols, code and data regions, comments. Code is
//! traced from the interrupt vectors and entry points; every byte is emitted,
//! so the source reassembles to the same ROM, which [`Disassembly::verify`]
//! checks by assembling it in memory.

mod analysis;
pub mod config;
mod render;

pub use config::{
    Address, BankLayout, ChrMode, CommentDef, CommentPosition, DisasmConfig, DisasmOptions,
    EntryPoint, LabelDef, OperandDef, ReferenceDef, RegionDef, RegionType, SymbolDef,
};

use crate::state::BANK_SIZE;
use crate::{AssembleOptions, AssembleRequest, Diagnostic, DiagnosticCode, SourceLocation};
use analysis::{Analysis, Class, Loc, Target};
use render::Rendered;
#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Mismatches listed by [`Disassembly::verify`].
const MAX_MISMATCHES: usize = 20;
/// Names listed per routine or RAM address.
const MAX_NAMES: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RomFormat {
    /// iNES ROM.
    Nes,
    /// Raw payload of whole 8 KiB banks.
    Bin,
    /// Raw bytes loaded at `load_address`.
    Fragment,
}

/// Something the annotations should settle, or a limit of the round trip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    /// Header fields or data NESASM cannot reproduce.
    Header,
    /// Code flow reaches a byte that is not an NMOS 6502 instruction.
    InvalidOpcode,
    /// Code enters the middle of an instruction, or would overlap other bytes.
    Overlap,
    /// Code reaches a range a region describes as data.
    CodeDataConflict,
    /// `JMP [addr]` whose targets are unknown.
    IndirectJump,
    /// A reference to an address that several banks hold.
    UnresolvedBank,
    /// Code outside the ROM (RAM, SRAM) is called or jumped to.
    OutsideRom,
    /// Code runs off the end of a bank.
    FlowLeavesRom,
    /// A write to a ROM address: usually a mapper register.
    MapperWrite,
    /// Bytes that no code reaches and no region describes.
    UnknownRange,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Issue {
    pub kind: IssueKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<usize>,
    pub address: Address,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum XrefKind {
    Call,
    Jump,
    Branch,
    Read,
    Write,
    /// An entry of a pointer or low/high byte table.
    Pointer,
}

/// One line of the generated source.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct SourceLine {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<usize>,
    /// CPU address of the line (CHR banks: offset in the bank).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<Address>,
    /// ROM bytes the line produces.
    pub size: usize,
    /// The first bytes as hex pairs (`..` when there are more).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub bytes: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct BankSummary {
    pub bank: usize,
    pub chr: bool,
    /// CPU address of the bank's first byte (PRG banks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<Address>,
    pub code_bytes: usize,
    pub data_bytes: usize,
    /// Bytes not reached by code nor described by a region.
    pub unknown_bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Summary {
    pub format: RomFormat,
    pub banks: Vec<BankSummary>,
    pub instructions: usize,
    pub labels: usize,
    pub routines: usize,
    /// Number of issues of each kind.
    pub issues: BTreeMap<IssueKind, usize>,
}

/// A routine: code reached from an entry point or call target without
/// following calls.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Routine {
    pub name: String,
    pub bank: usize,
    pub address: Address,
    /// Bytes of instructions.
    pub size: usize,
    /// Number of JSRs to the routine.
    pub callers: usize,
    /// Routines it calls.
    pub calls: Vec<String>,
    /// Addresses outside the ROM it reads and writes.
    pub reads: Vec<String>,
    pub writes: Vec<String>,
}

/// Accesses of an address outside the ROM (RAM, I/O, SRAM).
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct RamUse {
    pub address: Address,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub reads: usize,
    pub writes: usize,
    /// Routines that access it.
    pub routines: Vec<String>,
}

/// A reference to a queried address or name.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Xref {
    pub bank: usize,
    pub address: Address,
    pub kind: XrefKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routine: Option<String>,
    /// The referring source line.
    pub line: String,
}

/// The result of reassembling the generated source.
#[derive(Clone, Debug, Default, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Verification {
    /// The source reassembles to the same ROM.
    pub matched: bool,
    /// Assembly errors of the generated source.
    pub diagnostics: Vec<Diagnostic>,
    /// The first differing bytes.
    pub mismatches: Vec<Mismatch>,
    /// Number of differing bytes.
    pub mismatch_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Mismatch {
    /// ROM bank; absent for the iNES header (address is then the header offset).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<usize>,
    pub address: Address,
    pub expected: String,
    pub actual: String,
    /// The generated source line that produced the byte, as `file:line`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
}

/// Parses an annotation file.
pub fn parse_config(json: &str) -> Result<DisasmConfig, String> {
    serde_json::from_str(json).map_err(|e| format!("Invalid annotations: {e}"))
}

/// The default source path for a ROM: the same stem with `.asm`.
pub fn default_output(rom: &Path) -> PathBuf {
    rom.with_extension("asm")
}

/// The CHR file next to a source path.
pub fn chr_path(source: &Path) -> PathBuf {
    source.with_extension("chr")
}

fn error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(DiagnosticCode::Disasm, message, SourceLocation::default())
}

/// Disassembles `rom_file` (the bytes of `config.rom`) as annotated. `source`
/// is the path of the generated source relative to the working directory; a
/// CHR file is read from the same stem with `.chr`.
pub fn disassemble(
    rom_file: &[u8],
    config: &DisasmConfig,
    source: &Path,
    cancel: &AtomicBool,
) -> Result<Disassembly, Vec<Diagnostic>> {
    let analysis = Analysis::run(rom_file, config, cancel)
        .map_err(|errors| errors.into_iter().map(error).collect::<Vec<_>>())?;
    let rendered = render::render(&analysis, &config.rom.to_string_lossy(), source);
    Ok(Disassembly {
        analysis,
        rendered,
        source: source.to_owned(),
    })
}

/// Whether `text` is a source generated by [`disassemble`] and not edited
/// since (its marker line's checksum still matches).
pub fn is_unedited_source(text: &str) -> bool {
    render::is_unedited(text)
}

pub struct Disassembly {
    analysis: Analysis,
    rendered: Rendered,
    source: PathBuf,
}

impl Disassembly {
    /// The main source file's text (LF line endings).
    pub fn text(&self) -> &str {
        &self.rendered.files[0].1
    }

    /// The generated source files by path: the main file, then (when the
    /// source is split by bank) the bank files it includes.
    pub fn files(&self) -> &[(PathBuf, String)] {
        &self.rendered.files
    }

    pub fn lines(&self) -> &[SourceLine] {
        &self.rendered.lines
    }

    /// The source path given to [`disassemble`].
    pub fn source_path(&self) -> &Path {
        &self.source
    }

    /// The CHR file the source includes, with its contents.
    pub fn chr_file(&self) -> Option<(&Path, &[u8])> {
        self.rendered
            .chr
            .as_ref()
            .map(|(path, data)| (path.as_path(), data.as_slice()))
    }

    pub fn issues(&self) -> &[Issue] {
        &self.analysis.issues
    }

    pub fn summary(&self) -> Summary {
        let a = &self.analysis;
        let banks = a
            .memory
            .banks
            .iter()
            .map(|bank| {
                let classes = &a.class[bank.index][bank.start..bank.end];
                let count = |f: fn(&Class) -> bool| classes.iter().filter(|c| f(c)).count();
                BankSummary {
                    bank: bank.index,
                    chr: bank.chr,
                    org: (!bank.chr).then_some(Address(bank.org)),
                    code_bytes: if bank.chr {
                        0
                    } else {
                        count(|c| matches!(c, Class::Insn | Class::Operand))
                    },
                    data_bytes: if bank.chr {
                        bank.end - bank.start
                    } else {
                        count(|c| *c == Class::Data)
                    },
                    unknown_bytes: if bank.chr {
                        0
                    } else {
                        count(|c| *c == Class::Unknown)
                    },
                }
            })
            .collect();
        let mut issues = BTreeMap::new();
        for issue in &a.issues {
            *issues.entry(issue.kind).or_default() += 1;
        }
        Summary {
            format: a.memory.format,
            banks,
            instructions: a.insns.len(),
            labels: a.labels.values().map(Vec::len).sum(),
            routines: a.routines.len(),
            issues,
        }
    }

    /// Lines of `bank` (any bank if `None`) whose address is within `from..=to`.
    pub fn view(&self, bank: Option<usize>, from: u32, to: u32) -> Vec<&SourceLine> {
        self.rendered
            .lines
            .iter()
            .filter(|l| l.bank.is_some() && bank.is_none_or(|b| l.bank == Some(b)))
            .filter(|l| l.address.is_some_and(|a| a.0 >= from && a.0 <= to))
            .filter(|l| !self.is_chr_line(l))
            .collect()
    }

    fn is_chr_line(&self, line: &SourceLine) -> bool {
        line.bank.is_some_and(|b| self.analysis.memory.banks[b].chr)
    }

    fn name(&self, loc: Loc) -> String {
        self.analysis
            .rom_name(loc)
            .unwrap_or_else(|| Address(self.analysis.memory.address(loc)).to_string())
    }

    fn outside_name(&self, value: u32) -> String {
        self.analysis
            .symbol_name(value)
            .unwrap_or_else(|| Address(value).to_string())
    }

    pub fn routines(&self) -> Vec<Routine> {
        let a = &self.analysis;
        let mut callers: BTreeMap<Loc, usize> = BTreeMap::new();
        let mut by_source: BTreeMap<Loc, Vec<&analysis::Ref>> = BTreeMap::new();
        for r in &a.refs {
            if let (XrefKind::Call, Target::Rom(t)) = (r.kind, &r.target) {
                *callers.entry(*t).or_default() += 1;
            }
            by_source.entry(r.from).or_default().push(r);
        }
        a.routines
            .iter()
            .map(|(head, body)| {
                let mut calls = BTreeSet::new();
                let mut reads = BTreeSet::new();
                let mut writes = BTreeSet::new();
                for r in body
                    .iter()
                    .flat_map(|l| by_source.get(l).into_iter().flatten())
                {
                    match (r.kind, &r.target) {
                        (XrefKind::Call, Target::Rom(t)) => {
                            calls.insert(self.name(*t));
                        }
                        (XrefKind::Read, Target::Outside(v)) => {
                            reads.insert(self.outside_name(*v));
                        }
                        (XrefKind::Write, Target::Outside(v)) => {
                            writes.insert(self.outside_name(*v));
                        }
                        _ => {}
                    }
                }
                let cap = |set: BTreeSet<String>| set.into_iter().take(MAX_NAMES).collect();
                Routine {
                    name: self.name(*head),
                    bank: head.bank,
                    address: Address(a.memory.address(*head)),
                    size: body.iter().map(|l| a.insns[l].len).sum(),
                    callers: callers.get(head).copied().unwrap_or(0),
                    calls: cap(calls),
                    reads: cap(reads),
                    writes: cap(writes),
                }
            })
            .collect()
    }

    pub fn ram(&self) -> Vec<RamUse> {
        let a = &self.analysis;
        let mut uses: BTreeMap<u32, (usize, usize, BTreeSet<String>)> = BTreeMap::new();
        for r in &a.refs {
            let Target::Outside(value) = r.target else {
                continue;
            };
            let entry = uses.entry(value).or_default();
            match r.kind {
                XrefKind::Write => entry.1 += 1,
                XrefKind::Read | XrefKind::Pointer => entry.0 += 1,
                _ => {}
            }
            if let Some(head) = a.routine_of(r.from) {
                entry.2.insert(self.name(head));
            }
        }
        uses.into_iter()
            .map(|(value, (reads, writes, routines))| RamUse {
                address: Address(value),
                name: a.symbol_name(value),
                reads,
                writes,
                routines: routines.into_iter().take(MAX_NAMES).collect(),
            })
            .collect()
    }

    /// References to a label or symbol name, or to an address (`"$C000"`).
    pub fn xrefs(&self, query: &str) -> Result<Vec<Xref>, String> {
        let a = &self.analysis;
        let rom: BTreeSet<Loc> = a
            .labels
            .iter()
            .filter(|(_, labels)| labels.iter().any(|l| l.name == query))
            .map(|(loc, _)| *loc)
            .collect();
        let symbol = a.symbols.iter().find(|s| s.name == query);
        let address =
            if rom.is_empty() && symbol.is_none() {
                Some(Address::parse(query).map_err(|_| {
                    format!("'{query}' is neither a label, a symbol nor an address")
                })?)
            } else {
                None
            };
        let matches = |target: &Target| match target {
            Target::Rom(loc) => {
                rom.contains(loc) || address.is_some_and(|v| v.0 == a.memory.address(*loc))
            }
            Target::Ambiguous(v) => address.is_some_and(|a| a.0 == *v),
            Target::Outside(v) => {
                symbol.is_some_and(|s| *v >= s.value && *v < s.value + s.size)
                    || address.is_some_and(|a| a.0 == *v)
            }
        };
        let lines: BTreeMap<(usize, u32), &SourceLine> = self
            .rendered
            .lines
            .iter()
            .filter(|l| l.size > 0)
            .filter_map(|l| Some(((l.bank?, l.address?.0), l)))
            .collect();
        Ok(a.refs
            .iter()
            .filter(|r| matches(&r.target))
            .map(|r| {
                let at = a.memory.address(r.from);
                let line = lines
                    .range(..=(r.from.bank, at))
                    .next_back()
                    .filter(|((bank, start), l)| *bank == r.from.bank && at < start + l.size as u32)
                    .map(|(_, l)| l.text.trim().to_owned())
                    .unwrap_or_default();
                Xref {
                    bank: r.from.bank,
                    address: Address(at),
                    kind: r.kind,
                    routine: a.routine_of(r.from).map(|h| self.name(h)),
                    line,
                }
            })
            .collect())
    }

    /// Assembles the generated source in memory (with its CHR file) and
    /// compares the result with the original ROM.
    pub fn verify(
        &self,
        working_directory: &Path,
        allowed_root: Option<&Path>,
        cancel: &AtomicBool,
    ) -> Verification {
        let memory = &self.analysis.memory;
        let mut files = BTreeMap::new();
        for (path, text) in &self.rendered.files {
            files.insert(path.clone(), text.as_bytes().to_vec());
        }
        if let Some((path, data)) = self.chr_file() {
            files.insert(path.to_owned(), data.to_vec());
        }
        let request = AssembleRequest {
            input: self.source.clone(),
            working_directory: working_directory.to_owned(),
            include_paths: Vec::new(),
            allowed_root: allowed_root.map(Path::to_owned),
            options: AssembleOptions {
                raw: memory.format != RomFormat::Nes,
                ..AssembleOptions::default()
            },
            files,
            collect_lines: true,
        };
        let result = crate::assemble_with_cancel(&request, cancel);
        if !result.success {
            return Verification {
                diagnostics: result
                    .diagnostics
                    .into_iter()
                    .filter(Diagnostic::is_error)
                    .collect(),
                ..Verification::default()
            };
        }
        let base = crate::source::canonicalize(working_directory)
            .unwrap_or_else(|_| working_directory.to_owned());
        let mut mismatches = Vec::new();
        let mut count = 0;
        let mut differ =
            |bank: Option<usize>, address: u32, expected: Option<u8>, actual: Option<u8>| {
                count += 1;
                if mismatches.len() < MAX_MISMATCHES {
                    let show = |b: Option<u8>| b.map_or("none".into(), |b| format!("{b:02X}"));
                    let line = bank.and_then(|bank| {
                        result
                            .lines
                            .iter()
                            .find(|l| {
                                l.bank.map(usize::from) == Some(bank)
                                    && l.address <= address
                                    && address < l.address + l.size as u32
                            })
                            .map(|l| {
                                let file = l
                                    .location
                                    .file
                                    .strip_prefix(&base)
                                    .unwrap_or(&l.location.file);
                                format!("{}:{}", file.display(), l.location.line)
                            })
                    });
                    mismatches.push(Mismatch {
                        bank,
                        address: Address(address),
                        expected: show(expected),
                        actual: show(actual),
                        line,
                    });
                }
            };
        if memory.format == RomFormat::Nes {
            for i in 0..memory.header.len().max(result.header.len()) {
                let (e, a) = (memory.header.get(i).copied(), result.header.get(i).copied());
                if e != a {
                    differ(None, i as u32, e, a);
                }
            }
        }
        let mut expected = vec![None; result.binary.len()];
        for bank in &memory.banks {
            for offset in bank.start..bank.end {
                let index = bank.index * BANK_SIZE + offset;
                if index >= expected.len() {
                    expected.resize(index + 1, None);
                }
                expected[index] = Some(bank.data[offset]);
            }
        }
        for (index, e) in expected.iter().enumerate() {
            let a = result.binary.get(index).copied();
            let bank = index / BANK_SIZE;
            let offset = index % BANK_SIZE;
            let address = memory
                .banks
                .get(bank)
                .map_or(offset as u32, |b| b.org + offset as u32);
            // A fragment only covers part of its banks; the rest stays zero.
            let e = match (e, memory.format) {
                (None, RomFormat::Fragment) if a.is_some() => Some(0),
                _ => *e,
            };
            if e != a {
                differ(Some(bank), address, e, a);
            }
        }
        Verification {
            matched: count == 0,
            diagnostics: Vec::new(),
            mismatches,
            mismatch_count: count,
        }
    }
}

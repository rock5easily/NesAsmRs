//! Disassembly into reassemblable NESASM source.
//!
//! A [`DisasmConfig`] (the annotation file) names the ROM and records what is
//! known about it: labels, symbols, code and data regions, comments. Code is
//! traced from the interrupt vectors and entry points; every byte is emitted,
//! so the source reassembles to the same ROM, which [`Disassembly::verify`]
//! checks by assembling it in memory.

mod analysis;
pub mod config;
mod patch;
mod render;

pub use config::{
    Address, BankLayout, ChrMode, CommentDef, CommentPosition, DisasmConfig, DisasmOptions,
    EntryPoint, LabelDef, OperandDef, PatchDef, ReferenceDef, RegionDef, RegionType, SymbolDef,
};

use crate::state::BANK_SIZE;
use crate::{AssembleOptions, AssembleRequest, Diagnostic, DiagnosticCode, SourceLocation};
use analysis::{Analysis, Class, Loc, Target};
use patch::Patches;
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
    /// Offset and length in the bank of the bytes concerned.
    #[serde(skip)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub(crate) span: Option<(usize, usize)>,
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
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
    /// For a ROM hack: how it differs from the base ROM.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffSummary>,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct DiffSummary {
    /// Bytes that differ from the base ROM (including banks it lacks).
    pub changed_bytes: usize,
    /// Ranges written as source; the rest is included from the base ROM.
    pub ranges: usize,
    /// Bytes in those ranges.
    pub source_bytes: usize,
    /// Banks the base ROM does not have (an expanded ROM).
    pub new_banks: Vec<usize>,
}

/// A range of a ROM hack written as source: changed bytes, widened to whole
/// instructions and the `patches` of the annotations.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Diff {
    pub bank: usize,
    pub chr: bool,
    /// First and last CPU address (offsets in the bank for CHR banks).
    pub start: Address,
    pub end: Address,
    /// Bytes in the range that differ from the base ROM.
    pub changed_bytes: usize,
    pub code_bytes: usize,
    pub data_bytes: usize,
    pub unknown_bytes: usize,
    /// The value the base ROM held throughout the range, when it was free
    /// space (new code or data rather than a change).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_fill: Option<String>,
    /// Labels and routines in the range.
    pub labels: Vec<String>,
    pub routines: Vec<String>,
    /// References from outside the range into it, such as hooks into new code.
    pub referenced_from: Vec<Xref>,
    /// The first bytes of the ROM and of the base ROM in the range.
    pub bytes: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_bytes: String,
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
    /// The routine has more instructions than are followed (16384).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
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
    /// The referring source line; empty in a ROM hack's unchanged ranges.
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
    /// ROM bank; absent for the iNES header and trainer (address is then the
    /// offset in the file).
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
///
/// For a ROM hack, `base_file` holds the bytes of `config.base`: only the
/// ranges that differ (widened by `config.patches`) are disassembled, and the
/// source includes the rest from the base ROM.
pub fn disassemble(
    rom_file: &[u8],
    base_file: Option<&[u8]>,
    config: &DisasmConfig,
    source: &Path,
    cancel: &AtomicBool,
) -> Result<Disassembly, Vec<Diagnostic>> {
    let errors = |errors: Vec<String>| errors.into_iter().map(error).collect::<Vec<_>>();
    // Paths are written into `.include`/`.incbin` strings.
    for path in [Some(source), config.base.as_deref()].into_iter().flatten() {
        if path
            .to_string_lossy()
            .chars()
            // A backslash is a separator only on Windows.
            .any(|c| c == '"' || c.is_control() || (c == '\\' && !cfg!(windows)))
        {
            return Err(vec![error(format!(
                "Path '{}' contains a quote, control character or backslash",
                path.display()
            ))]);
        }
    }
    let mut analysis = Analysis::run(rom_file, config, cancel).map_err(errors)?;
    let patches = match (base_file, &config.base) {
        (Some(base), Some(_)) => Some(Patches::new(&analysis, base, config).map_err(errors)?),
        (None, None) => {
            if !config.patches.is_empty() {
                return Err(vec![error("patches applies only with base")]);
            }
            None
        }
        (None, Some(_)) => return Err(vec![error("The base ROM was not given")]),
        (Some(_), None) => return Err(vec![error("A base ROM was given without base")]),
    };
    if let Some(patches) = &patches {
        // Only the changed ranges matter.
        analysis
            .issues
            .retain(|issue| match (issue.bank, issue.span) {
                (Some(bank), Some((start, length))) => patches.ranges[bank]
                    .iter()
                    .any(|r| start < r.end && start + length > r.start),
                _ => true,
            });
    }
    let rendered = render::render(
        &analysis,
        &config.rom.to_string_lossy(),
        source,
        patches.as_ref(),
    );
    Ok(Disassembly {
        analysis,
        rendered,
        patches,
        source: source.to_owned(),
    })
}

/// Whether `text` is a source generated by [`disassemble`] and not edited
/// since (its marker line's checksum still matches).
pub fn is_unedited_source(text: &str) -> bool {
    render::is_unedited(text)
}

/// Whether `chr` is the CHR file the generated main source `text` records
/// (it was not changed since).
pub fn is_unedited_chr(text: &str, chr: &[u8]) -> bool {
    render::is_unedited_chr(text, chr)
}

pub struct Disassembly {
    analysis: Analysis,
    rendered: Rendered,
    patches: Option<Patches>,
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
            diff: self.patches.as_ref().map(|p| DiffSummary {
                changed_bytes: p.changed.iter().flatten().filter(|c| **c).count(),
                ranges: p.ranges.iter().map(Vec::len).sum(),
                source_bytes: p.ranges.iter().flatten().map(|r| r.end - r.start).sum(),
                new_banks: (0..p.base_bank.len())
                    .filter(|b| p.base_bank[*b].is_none())
                    .collect(),
            }),
        }
    }

    /// For a ROM hack, the ranges written as source, with what is known about
    /// each; empty without a base ROM.
    pub fn diffs(&self) -> Vec<Diff> {
        let Some(p) = &self.patches else {
            return Vec::new();
        };
        let a = &self.analysis;
        let lines = self.line_map();
        let mut by_target: BTreeMap<Loc, Vec<&analysis::Ref>> = BTreeMap::new();
        for r in &a.refs {
            if let Target::Rom(t) = r.target {
                by_target.entry(t).or_default().push(r);
            }
        }
        let mut diffs = Vec::new();
        for (bank, ranges) in p.ranges.iter().enumerate() {
            let image = &a.memory.banks[bank];
            for range in ranges {
                let (start, end) = patch::bounds(a, bank, range);
                let classes = &a.class[bank][range.start..range.end];
                let count = |f: fn(&Class) -> bool| {
                    if image.chr {
                        // CHR banks are data, as in the bank summary.
                        if f(&Class::Data) { classes.len() } else { 0 }
                    } else {
                        classes.iter().filter(|c| f(c)).count()
                    }
                };
                let base = p.base_bank[bank].map(|b| &p.base.banks[b]);
                let base_data = base.map_or(&[][..], |b| {
                    &b.data[range.start.max(b.start).min(b.end)..range.end.min(b.end)]
                });
                let base_fill = (base_data.len() == range.end - range.start
                    && base_data.iter().all(|b| *b == base_data[0]))
                .then(|| format!("{:02X}", base_data[0]));
                let inside = |loc: &Loc| {
                    loc.bank == bank && loc.offset >= range.start && loc.offset < range.end
                };
                let labels: Vec<String> = a
                    .labels
                    .range(
                        Loc {
                            bank,
                            offset: range.start,
                        }..Loc {
                            bank,
                            offset: range.end,
                        },
                    )
                    .flat_map(|(_, labels)| labels.iter().map(|l| l.name.clone()))
                    .take(MAX_NAMES)
                    .collect();
                let routines: BTreeSet<String> = a
                    .insns
                    .range(
                        Loc {
                            bank,
                            offset: range.start,
                        }..Loc {
                            bank,
                            offset: range.end,
                        },
                    )
                    .filter_map(|(loc, _)| a.routine_of(*loc))
                    .map(|head| self.name(head))
                    .collect();
                let referenced_from = by_target
                    .range(
                        Loc {
                            bank,
                            offset: range.start,
                        }..Loc {
                            bank,
                            offset: range.end,
                        },
                    )
                    .flat_map(|(_, refs)| refs.iter())
                    .filter(|r| !inside(&r.from))
                    .take(MAX_NAMES)
                    .map(|r| self.xref(r, &lines))
                    .collect();
                let shown = 16.min(range.end - range.start);
                diffs.push(Diff {
                    bank,
                    chr: image.chr,
                    start,
                    end,
                    changed_bytes: p.changed[bank][range.start..range.end]
                        .iter()
                        .filter(|c| **c)
                        .count(),
                    code_bytes: count(|c| matches!(c, Class::Insn | Class::Operand)),
                    data_bytes: count(|c| *c == Class::Data),
                    unknown_bytes: count(|c| *c == Class::Unknown),
                    base_fill,
                    labels,
                    routines: routines.into_iter().take(MAX_NAMES).collect(),
                    referenced_from,
                    bytes: crate::inspect::hex(&image.data[range.start..range.start + shown]),
                    base_bytes: crate::inspect::hex(&base_data[..shown.min(base_data.len())]),
                });
            }
        }
        diffs
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
                    truncated: a.truncated.contains(head),
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
            for head in a.routines_of(r.from) {
                entry.2.insert(self.name(*head));
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
        let lines = self.line_map();
        Ok(a.refs
            .iter()
            .filter(|r| matches(&r.target))
            .map(|r| self.xref(r, &lines))
            .collect())
    }

    /// Source lines that produce bytes, by bank and address.
    fn line_map(&self) -> BTreeMap<(usize, u32), &SourceLine> {
        self.rendered
            .lines
            .iter()
            .filter(|l| l.size > 0)
            .filter_map(|l| Some(((l.bank?, l.address?.0), l)))
            .collect()
    }

    fn xref(&self, r: &analysis::Ref, lines: &BTreeMap<(usize, u32), &SourceLine>) -> Xref {
        let a = &self.analysis;
        let at = a.memory.address(r.from);
        // A ROM hack's unchanged ranges have no source lines.
        let included = self
            .patches
            .as_ref()
            .is_some_and(|p| p.range_at(r.from).is_none());
        let line = lines
            .range(..=(r.from.bank, at))
            .next_back()
            .filter(|_| !included)
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
            // The source cannot produce a trainer.
            for (i, byte) in memory.trainer_bytes.iter().enumerate() {
                differ(
                    None,
                    (crate::inspect::HEADER_SIZE + i) as u32,
                    Some(*byte),
                    None,
                );
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

//! ROM hacks: the bytes that differ from the base ROM, widened into the
//! ranges the source disassembles; everything else is included from the base.

use super::analysis::{Analysis, Class, Loc, Memory};
use super::config::{DisasmConfig, RegionType};
use super::{Address, RomFormat};
use crate::inspect::HEADER_SIZE;
use crate::state::BANK_SIZE;

/// Differences closer than this are one range.
const MERGE_GAP: usize = 4;

/// A range of one bank written as source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PatchRange {
    pub start: usize,
    pub end: usize,
    pub comment: Option<String>,
}

pub(crate) struct Patches {
    /// The base ROM path as written in `.incbin`.
    pub path: String,
    pub base: Memory,
    /// Header bytes before the base ROM's payload.
    pub header_len: usize,
    /// For each bank of the ROM: the base bank with the same role (PRG or
    /// CHR, same index among them), if the base has one.
    pub base_bank: Vec<Option<usize>>,
    /// Source ranges of each bank, sorted and disjoint.
    pub ranges: Vec<Vec<PatchRange>>,
    /// Bytes that differ from the base, per bank.
    pub changed: Vec<Vec<bool>>,
}

impl Patches {
    /// Compares the analysed ROM with the base and decides the ranges.
    pub fn new(a: &Analysis, base_file: &[u8], config: &DisasmConfig) -> Result<Self, Vec<String>> {
        let path = config
            .base
            .as_ref()
            .map_or(String::new(), |p| p.to_string_lossy().replace('\\', "/"));
        let memory = &a.memory;
        if memory.format == RomFormat::Fragment {
            return Err(vec!["base cannot be combined with load_address".into()]);
        }
        // Only the bytes are compared, so the base keeps the default layout
        // (`banks` may name banks only an expanded hack has).
        let base = Memory::new(base_file, &DisasmConfig::default())
            .map_err(|e| vec![format!("base: {e}")])?;
        if base.format != memory.format {
            return Err(vec![
                "base must have the same format as rom (both .nes or both raw .bin)".into(),
            ]);
        }
        if base.trainer {
            return Err(vec![
                "base has a trainer, which the source cannot include".into(),
            ]);
        }
        let header_len = if base.format == RomFormat::Nes {
            HEADER_SIZE
        } else {
            0
        };
        // Banks are matched by role: PRG to PRG and CHR to CHR in order, so a
        // hack with more PRG banks still lines up its CHR banks.
        let base_prg: Vec<usize> = base
            .banks
            .iter()
            .filter(|b| !b.chr)
            .map(|b| b.index)
            .collect();
        let base_chr: Vec<usize> = base
            .banks
            .iter()
            .filter(|b| b.chr)
            .map(|b| b.index)
            .collect();
        let (mut prg, mut chr) = (0, 0);
        let base_bank: Vec<Option<usize>> = memory
            .banks
            .iter()
            .map(|b| {
                let (list, n) = if b.chr {
                    (&base_chr, &mut chr)
                } else {
                    (&base_prg, &mut prg)
                };
                *n += 1;
                list.get(*n - 1).copied()
            })
            .collect();
        let changed: Vec<Vec<bool>> = memory
            .banks
            .iter()
            .map(|bank| {
                let other = base_bank[bank.index].map(|b| &base.banks[b]);
                (0..BANK_SIZE)
                    .map(|o| {
                        o >= bank.start
                            && o < bank.end
                            && other.is_none_or(|b| {
                                o < b.start || o >= b.end || b.data[o] != bank.data[o]
                            })
                    })
                    .collect()
            })
            .collect();
        let mut ranges: Vec<Vec<PatchRange>> = memory.banks.iter().map(|_| Vec::new()).collect();
        for bank in &memory.banks {
            let flags = &changed[bank.index];
            if bank.chr {
                // CHR banks are written whole.
                if flags.iter().any(|c| *c) {
                    ranges[bank.index].push(PatchRange {
                        start: bank.start,
                        end: bank.end,
                        comment: None,
                    });
                }
                continue;
            }
            let mut offset = bank.start;
            while offset < bank.end {
                if !flags[offset] {
                    offset += 1;
                    continue;
                }
                let start = offset;
                let mut end = offset + 1;
                while let Some(next) = (end..(end + MERGE_GAP).min(bank.end)).find(|o| flags[*o]) {
                    end = next + 1;
                }
                ranges[bank.index].push(PatchRange {
                    start,
                    end,
                    comment: None,
                });
                offset = end;
            }
        }
        let mut errors = Vec::new();
        for (i, def) in config
            .patches
            .iter()
            .enumerate()
            .filter(|(_, e)| config.applies(*e))
        {
            match memory.span(def.bank, def.start, def.end, def.length) {
                Ok((loc, end)) => ranges[loc.bank].push(PatchRange {
                    start: loc.offset,
                    end,
                    comment: def.comment.clone(),
                }),
                Err(e) => errors.push(format!("patches[{i}] {}: {e}", def.start)),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        widen(a, &mut ranges);
        Ok(Self {
            path,
            base,
            header_len,
            base_bank,
            ranges,
            changed,
        })
    }

    /// The source range holding `loc`.
    pub fn range_at(&self, loc: Loc) -> Option<&PatchRange> {
        self.ranges[loc.bank]
            .iter()
            .find(|r| r.start <= loc.offset && loc.offset < r.end)
    }
}

/// Widens the ranges of the PRG banks to whole instructions and table
/// entries, then merges overlapping and adjacent ones, until nothing
/// changes. An instruction across the banks of the fixed pair widens a
/// range of each bank to the bank boundary.
fn widen(a: &Analysis, ranges: &mut [Vec<PatchRange>]) {
    loop {
        let before = ranges.to_vec();
        // Ranges of the other bank of the fixed pair.
        let mut added: Vec<(usize, usize, usize)> = Vec::new();
        for (bank, list) in ranges.iter_mut().enumerate() {
            if a.memory.banks[bank].chr {
                continue;
            }
            let end_of_bank = a.memory.banks[bank].end;
            for range in list.iter_mut() {
                let loc = Loc {
                    bank,
                    offset: range.start,
                };
                if a.class[bank][range.start] == Class::Operand
                    && let Some(start) = a.containing(loc)
                {
                    if start.bank == bank {
                        range.start = start.offset;
                    } else {
                        range.start = 0;
                        let other = a.memory.banks[start.bank].end;
                        added.push((start.bank, start.offset, other));
                    }
                }
                let last = loc.at(range.end - 1);
                let insn = match a.class[bank][last.offset] {
                    Class::Insn => Some(last),
                    Class::Operand => a.containing(last),
                    _ => None,
                };
                if let Some(start) = insn
                    && let Some(end) = a.memory.step(start, a.insns[&start].len - 1)
                {
                    if end.bank == bank {
                        range.end = range.end.max(end.offset + 1);
                    } else {
                        range.end = end_of_bank;
                        added.push((end.bank, 0, end.offset + 1));
                    }
                }
                for edge in [range.start, range.end - 1] {
                    if let Some(region) = a.region_at(loc.at(edge))
                        && matches!(region.kind, RegionType::Words | RegionType::Pointers)
                    {
                        let pair = region.start + (edge - region.start) / 2 * 2;
                        range.start = range.start.min(pair);
                        range.end = range.end.max((pair + 2).min(region.end));
                    }
                }
            }
        }
        for (bank, start, end) in added {
            ranges[bank].push(PatchRange {
                start,
                end,
                comment: None,
            });
        }
        for list in ranges.iter_mut() {
            list.sort_by_key(|r| r.start);
            let mut merged: Vec<PatchRange> = Vec::new();
            for range in list.drain(..) {
                match merged.last_mut() {
                    Some(last) if range.start <= last.end => {
                        last.end = last.end.max(range.end);
                        if last.comment.is_none() {
                            last.comment = range.comment;
                        }
                    }
                    _ => merged.push(range),
                }
            }
            *list = merged;
        }
        if *ranges == *before {
            return;
        }
    }
}

/// Converts an offset range to inclusive CPU addresses.
pub(crate) fn bounds(a: &Analysis, bank: usize, range: &PatchRange) -> (Address, Address) {
    let org = if a.memory.banks[bank].chr {
        0
    } else {
        a.memory.banks[bank].org
    };
    (
        Address(org + range.start as u32),
        Address(org + range.end as u32 - 1),
    )
}

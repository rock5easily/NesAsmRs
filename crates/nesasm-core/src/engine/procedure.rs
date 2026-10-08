//! PROC / PROCGROUP relocation and CALL trampolines.

use super::{Cursor, Engine, Position};
use crate::error::{AsmError, AsmResult};
use crate::source::Line;
use crate::state::{BANK_SIZE, CALL_PAGE, MAX_BANKS, PROC_PAGE, PROCEDURE_BANK, Section};
use crate::{BankRef, DiagnosticCode, SourceLocation};
use std::{
    collections::{BTreeMap, HashMap},
    rc::Rc,
};

/// CPU address procedures are assembled for ($A000, page 5).
const PROC_BASE: usize = PROC_PAGE * BANK_SIZE;
/// CPU address of the trampoline bank ($8000, page 4).
const CALL_BASE: usize = CALL_PAGE * BANK_SIZE;
const TRAMPOLINE_SIZE: usize = 18;

struct Procedure {
    /// Offset in the enclosing group (0 for a top-level procedure).
    base: usize,
    bank: usize,
    org: usize,
    size: usize,
    group: Option<usize>,
    location: SourceLocation,
}

struct ProcFrame {
    id: usize,
    saved: Cursor,
    group: bool,
    location: SourceLocation,
}

#[derive(Default)]
pub(super) struct Procedures {
    list: Vec<Procedure>,
    index: HashMap<String, usize>,
    frames: Vec<ProcFrame>,
    /// Symbols defined inside a procedure, relocated with it.
    symbols: BTreeMap<String, usize>,
    /// Unnamed PROCGROUP counter, reset each pass so both passes agree on names.
    unnamed_groups: usize,
    call_bank: Option<usize>,
    trampolines: usize,
    calls: HashMap<String, u32>,
}

impl Procedures {
    pub fn reset(&mut self) {
        self.frames.clear();
        self.unnamed_groups = 0;
    }
    /// Whether a procedure or group is open.
    pub fn inside(&self) -> bool {
        !self.frames.is_empty()
    }
    pub fn open_location(&self) -> Option<SourceLocation> {
        self.frames.last().map(|f| f.location.clone())
    }
    /// Remembers that `symbol` belongs to the innermost open procedure.
    pub fn register_symbol(&mut self, symbol: &str) {
        if let Some(frame) = self.frames.last() {
            self.symbols.insert(symbol.into(), frame.id);
        }
    }
}

impl Engine<'_> {
    pub(super) fn begin_proc(
        &mut self,
        label: Option<&str>,
        operand: &str,
        line: &Line,
        group: bool,
    ) -> AsmResult<()> {
        if self.section != Section::Code {
            return Err("Procedure requires CODE section".into());
        }
        if self
            .procs
            .frames
            .last()
            .is_some_and(|frame| group || !frame.group)
        {
            return Err("Cannot nest procedures/groups".into());
        }
        let mut name = label.unwrap_or(operand).trim().to_string();
        if name.is_empty() && group {
            self.procs.unnamed_groups += 1;
            name = format!("__group_{}__", self.procs.unnamed_groups);
        }
        if name.is_empty() || name.starts_with('.') {
            return Err("Invalid procedure name".into());
        }
        if self.pass.is_layout() {
            if self.procs.index.contains_key(&name) {
                return Err("Duplicate procedure".into());
            }
            let base = if self.procs.inside() {
                self.position.offset
            } else {
                0
            };
            let parent = self.procs.frames.last().map(|f| f.id);
            self.procs.index.insert(name.clone(), self.procs.list.len());
            self.procs.list.push(Procedure {
                base,
                org: base,
                bank: PROCEDURE_BANK,
                size: 0,
                group: parent,
                location: line.location(),
            });
        }
        let id = *self.procs.index.get(&name).ok_or("Procedure not found")?;
        let saved = Cursor {
            position: self.position,
            scope: Rc::clone(&self.scope),
        };
        let p = &self.procs.list[id];
        self.position = Position {
            bank: p.bank,
            page: PROC_PAGE,
            offset: p.org,
        };
        self.scope = Rc::from(name.as_str());
        self.procs.frames.push(ProcFrame {
            id,
            saved,
            group,
            location: line.location(),
        });
        self.define(&name, self.pc(), line, false)?;
        Ok(())
    }

    pub(super) fn end_proc(&mut self, group: bool) -> AsmResult<()> {
        let frame = self.procs.frames.pop().ok_or("Unexpected procedure end")?;
        if frame.group != group {
            return Err("Mismatched procedure end".into());
        }
        let end = self.position.offset;
        if self.pass.is_layout() {
            let p = &mut self.procs.list[frame.id];
            p.size = end
                .checked_sub(p.base)
                .ok_or_else(|| AsmError::fatal("Procedure too large"))?;
            if p.size > BANK_SIZE {
                return Err(AsmError::fatal("Procedure too large"));
            }
        }
        self.position = frame.saved.position;
        self.scope = frame.saved.scope;
        if self.procs.inside() {
            self.position.offset = end;
        }
        Ok(())
    }

    /// Places procedures in the banks after the code, after the layout pass,
    /// and moves the symbols defined inside them.
    pub(super) fn relocate(&mut self) {
        if self.procs.list.is_empty() {
            return;
        }
        let mut bank = self.rom.max_bank + 1;
        let mut offset = 0;
        let mut overflow = None;
        for i in 0..self.procs.list.len() {
            if let Some(parent) = self.procs.list[i].group {
                let (parent_org, parent_base, parent_bank) = {
                    let g = &self.procs.list[parent];
                    (g.org, g.base, g.bank)
                };
                let p = &mut self.procs.list[i];
                p.org = p.base + parent_org - parent_base;
                p.bank = parent_bank;
            } else {
                let p = &mut self.procs.list[i];
                if offset + p.size > BANK_SIZE {
                    bank += 1;
                    offset = 0;
                }
                p.bank = bank;
                p.org = offset;
                offset += p.size;
                if bank >= MAX_BANKS && overflow.is_none() {
                    overflow = Some(p.location.clone());
                }
            }
        }
        if let Some(at) = overflow {
            self.error_at(
                &at,
                &[],
                DiagnosticCode::Procedure,
                "Not enough ROM space for procedures",
            );
            return;
        }
        self.rom.max_bank = bank;
        for (name, &id) in &self.procs.symbols {
            let p = &self.procs.list[id];
            // Constants (EQU/RS) and RAM labels are not procedure addresses.
            if let Some(s) = self.result.symbols.get_mut(name)
                && s.bank != BankRef::Constant
            {
                s.value = s
                    .value
                    .wrapping_add(p.org as u32)
                    .wrapping_sub(p.base as u32);
                s.bank = BankRef::Rom(p.bank as u8);
            }
        }
        self.reserve("_call_bank", (bank + 1) as u32);
    }

    /// CALL: a JSR to the procedure, through a bank-switching trampoline when
    /// it lives in another bank.
    pub(super) fn call(&mut self, name: &str) -> AsmResult<Vec<u8>> {
        if self.pass.is_layout() {
            return Ok(vec![0x20, 0, 0]);
        }
        let target = match self.procs.index.get(name) {
            None => self.value(name)?,
            Some(&id) => {
                let (bank, org) = (self.procs.list[id].bank, self.procs.list[id].org);
                if self.position.bank == bank {
                    (PROC_BASE + org) as u32
                } else if let Some(addr) = self.procs.calls.get(name) {
                    *addr
                } else {
                    self.trampoline(name, bank, org)?
                }
            }
        };
        Ok(vec![0x20, target as u8, (target >> 8) as u8])
    }

    /// Writes a trampoline into the call bank and returns its address.
    fn trampoline(&mut self, name: &str, bank: usize, org: usize) -> AsmResult<u32> {
        let call_bank = match self.procs.call_bank {
            Some(b) => b,
            None => {
                let b = self.rom.max_bank + 1;
                if b >= MAX_BANKS {
                    return Err(AsmError::fatal("Call bank exceeds ROM limit"));
                }
                self.procs.call_bank = Some(b);
                self.rom.max_bank = b;
                b
            }
        };
        let offset = self.procs.trampolines;
        if offset + TRAMPOLINE_SIZE > BANK_SIZE {
            return Err(AsmError::fatal("Call bank overflow"));
        }
        let target = PROC_BASE + org;
        let stub: [u8; TRAMPOLINE_SIZE] = [
            0xa8,
            0x43,
            0x20,
            0x48,
            0xa9,
            bank as u8,
            0x53,
            0x20,
            0x98,
            0x20,
            target as u8,
            (target >> 8) as u8,
            0xa8,
            0x68,
            0x53,
            0x20,
            0x98,
            0x60,
        ];
        self.rom.write(
            call_bank * BANK_SIZE + offset,
            &stub,
            Section::Code.map_byte(CALL_PAGE),
        );
        self.procs.trampolines += TRAMPOLINE_SIZE;
        let addr = (CALL_BASE + offset) as u32;
        self.procs.calls.insert(name.into(), addr);
        Ok(addr)
    }
}

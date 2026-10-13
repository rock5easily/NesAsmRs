//! What is known along a path of code: constant register, RAM and stack
//! values, and the PRG banks switched in at $8000 and $A000; and summaries
//! of what a called routine does to them, found by following each of its
//! paths with symbolic values.

use super::analysis::{Insn, Loc};
use crate::opcode::{Mnemonic, Mode};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// RAM values written along a path that are remembered.
const MAX_MEMORY: usize = 32;
/// Values pushed along a path that are remembered.
const MAX_STACK: usize = 16;
/// Instructions followed to summarise one routine, over all its paths.
const MAX_SUMMARY_STEPS: usize = 4096;
/// Instructions scanned to tell that a routine never writes to the mapper.
const MAX_SCAN: usize = 16384;
/// Nested calls followed by a summary.
const MAX_DEPTH: usize = 6;

/// Whether the values of `address` are followed: internal RAM and the
/// $6000-$7FFF work RAM.
fn tracked(address: u32) -> bool {
    address < 0x800 || (0x6000..0x8000).contains(&address)
}

/// A value at the start of a summarised routine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Input {
    A,
    X,
    Y,
    Mem(u16),
    /// A byte pulled from below the routine's own pushes: its return
    /// address (0 the low byte, 1 the high byte).
    Return(u8),
    /// A byte read through a pointer holding the return address: from the
    /// bytes after the JSR.
    AfterCall,
}

/// A byte value: a constant, an input plus a constant, or unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Sym {
    #[default]
    Unknown,
    Const(u8),
    Input(Input, u8),
}

impl Sym {
    pub fn constant(self) -> Option<u8> {
        match self {
            Self::Const(c) => Some(c),
            _ => None,
        }
    }

    fn add(self, n: u8) -> Self {
        match self {
            Self::Const(c) => Self::Const(c.wrapping_add(n)),
            Self::Input(i, k) => Self::Input(i, k.wrapping_add(n)),
            Self::Unknown => Self::Unknown,
        }
    }

    fn is_return(self) -> bool {
        matches!(self, Self::Input(Input::Return(_), _))
    }

    fn is_after_call(self) -> bool {
        matches!(self, Self::Input(Input::AfterCall, _))
    }

    fn meet(self, other: Self) -> Self {
        if self == other { self } else { Self::Unknown }
    }
}

/// The number of distinct lost ranges of two states.
fn lost_count(a: &[(u16, u16)], b: &[(u16, u16)]) -> usize {
    a.len() + b.iter().filter(|r| !a.contains(r)).count()
}

fn meet_option(a: Option<Sym>, b: Option<Sym>) -> Option<Sym> {
    if a == b { a } else { Some(Sym::Unknown) }
}

/// Values known at a point of a path. In a path traced from the vectors
/// and entry points, unknown values are [`Sym::Unknown`] and `windows`
/// `None`; in a summary, values are in terms of the routine's inputs and
/// `None` means unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct State {
    pub a: Sym,
    pub x: Sym,
    pub y: Sym,
    carry: Option<bool>,
    /// RAM written along the path, in the order written.
    memory: Vec<(u16, Sym)>,
    /// RAM not written yet holds its value at the start of the routine
    /// (summaries, until an unknown call).
    entry_memory: bool,
    /// RAM written in unknown ways (indexed writes): not written values are
    /// unknown there.
    lost: Vec<(u16, u16)>,
    /// Values pushed along the path, the last on top.
    pub stack: Vec<Sym>,
    /// Bytes pulled from below the path's own pushes.
    pub popped: u8,
    /// Pushes and pulls no longer pair up (TXS, too many values).
    stack_lost: bool,
    /// The MMC3 bank select value.
    select: Option<Sym>,
    /// The PRG banks (8 KiB) at $8000 and $A000.
    pub windows: [Option<Sym>; 2],
    /// The return address was kept: stored or pushed again.
    pub return_used: bool,
    /// The code wrote the bank of a window to the mapper itself (not
    /// through a call): code in a switched bank does not assume its own
    /// bank is still there.
    pub switched: [bool; 2],
}

/// What an instruction did that the analysis reports.
#[derive(Clone, Debug, Default)]
pub(crate) struct Effect {
    /// A RAM byte written with a known value.
    pub stored: Option<u16>,
    /// An MMC3 register write, explained.
    pub mapper: Option<String>,
}

impl State {
    /// The state at the start of a summarised routine, called with the
    /// banks `windows` switched in.
    fn routine(windows: [Option<u8>; 2]) -> Self {
        Self {
            a: Sym::Input(Input::A, 0),
            x: Sym::Input(Input::X, 0),
            y: Sym::Input(Input::Y, 0),
            entry_memory: true,
            windows: windows.map(|w| w.map(Sym::Const)),
            ..Self::default()
        }
    }

    /// The state a called routine starts with on a traced path: only the
    /// banks are known.
    pub fn call(&self) -> Self {
        Self {
            windows: self.windows,
            ..Self::default()
        }
    }

    /// The bank known at `window` (0 for $8000, 1 for $A000).
    pub fn window(&self, window: usize) -> Option<u8> {
        self.windows[window].and_then(Sym::constant)
    }

    /// The constant banks at $8000 and $A000.
    pub fn known_windows(&self) -> [Option<u8>; 2] {
        [self.window(0), self.window(1)]
    }

    pub fn set_window(&mut self, window: usize, bank: u8) {
        self.windows[window] = Some(Sym::Const(bank));
    }

    pub fn read(&self, address: u32) -> Sym {
        if !tracked(address) {
            return Sym::Unknown;
        }
        let address = address as u16;
        match self.memory.iter().rev().find(|(a, _)| *a == address) {
            Some((_, value)) => *value,
            None if self.entry_memory
                && !self.lost.iter().any(|(l, h)| (*l..=*h).contains(&address)) =>
            {
                Sym::Input(Input::Mem(address), 0)
            }
            None => Sym::Unknown,
        }
    }

    fn write(&mut self, address: u32, value: Sym) {
        if !tracked(address) {
            return;
        }
        let address = address as u16;
        self.memory.retain(|(a, _)| *a != address);
        if self.memory.len() == MAX_MEMORY {
            // A value no longer remembered is unknown, not the one at the
            // start of the routine.
            self.memory.remove(0);
            self.entry_memory = false;
        }
        self.memory.push((address, value));
        if value.is_return() {
            self.return_used = true;
        }
    }

    /// RAM from `low` to `high` may have been written with unknown values.
    fn clobber(&mut self, low: u32, high: u32) {
        let (low, high) = (low.min(0xffff) as u16, high.min(0xffff) as u16);
        self.memory.retain(|(a, _)| !(low..=high).contains(a));
        if !self.lost.contains(&(low, high)) {
            if self.lost.len() == MAX_STACK {
                self.lost.clear();
                self.entry_memory = false;
            }
            self.lost.push((low, high));
        }
    }

    /// All RAM may have been written with unknown values.
    fn clobber_all(&mut self) {
        self.memory.clear();
        self.lost.clear();
        self.entry_memory = false;
    }

    /// The known value of the 16-bit pointer at `address`.
    pub fn word(&self, address: u32) -> Option<u16> {
        let low = self.read(address).constant()?;
        let high = self.read(address + 1).constant()?;
        Some(u16::from_le_bytes([low, high]))
    }

    /// The address `JMP [address]` reads: the NMOS 6502 takes the high byte
    /// from the same page.
    pub fn jump_word(&self, address: u32) -> Option<u16> {
        let (low, high) = Self::jump_bytes(address);
        let low = self.read(low).constant()?;
        let high = self.read(high).constant()?;
        Some(u16::from_le_bytes([low, high]))
    }

    /// The addresses of the low and high byte `JMP [address]` reads.
    pub fn jump_bytes(address: u32) -> (u32, u32) {
        (address, (address & 0xff00) | ((address + 1) & 0xff))
    }

    fn push(&mut self, value: Sym) {
        if value.is_return() {
            self.return_used = true;
        }
        if self.stack.len() == MAX_STACK {
            self.stack.remove(0);
            self.stack_lost = true;
        }
        self.stack.push(value);
    }

    fn pull(&mut self) -> Sym {
        if let Some(value) = self.stack.pop() {
            return value;
        }
        if self.stack_lost {
            return Sym::Unknown;
        }
        self.popped = self.popped.saturating_add(1);
        Sym::Input(Input::Return(self.popped - 1), 0)
    }

    /// The two values pushed last, as the address an RTS would return to.
    pub fn pushed_word(&self) -> Option<u16> {
        match self.stack.as_slice() {
            [.., high, low] => Some(u16::from_le_bytes([low.constant()?, high.constant()?])),
            _ => None,
        }
    }

    /// What is known on both `self` and `other`.
    pub fn meet(&self, other: &Self) -> Self {
        // RAM written on one path only holds an unknown value.
        let mut memory: Vec<(u16, Sym)> = Vec::new();
        for (address, _) in self.memory.iter().chain(&other.memory) {
            if memory.iter().any(|(a, _)| a == address) {
                continue;
            }
            let value = self
                .read(u32::from(*address))
                .meet(other.read(u32::from(*address)));
            memory.push((*address, value));
        }
        if !(self.entry_memory && other.entry_memory) {
            // Not written is unknown too.
            memory.retain(|(_, v)| *v != Sym::Unknown);
        }
        // Values dropped are unknown, not the ones at the start.
        let overflow = memory.len() > MAX_MEMORY || lost_count(&self.lost, &other.lost) > MAX_STACK;
        if memory.len() > MAX_MEMORY {
            memory.drain(..memory.len() - MAX_MEMORY);
        }
        let mut lost = self.lost.clone();
        for range in &other.lost {
            if !lost.contains(range) {
                lost.push(*range);
            }
        }
        // Stacks of the same shape keep their positions, with the values
        // both paths pushed there.
        let same_stack = self.stack.len() == other.stack.len()
            && self.popped == other.popped
            && self.stack_lost == other.stack_lost;
        Self {
            a: self.a.meet(other.a),
            x: self.x.meet(other.x),
            y: self.y.meet(other.y),
            carry: if self.carry == other.carry {
                self.carry
            } else {
                None
            },
            entry_memory: self.entry_memory && other.entry_memory && !overflow,
            memory,
            lost: if lost.len() <= MAX_STACK {
                lost
            } else {
                Vec::new()
            },
            stack: if same_stack {
                self.stack
                    .iter()
                    .zip(&other.stack)
                    .map(|(a, b)| a.meet(*b))
                    .collect()
            } else {
                Vec::new()
            },
            popped: if same_stack { self.popped } else { 0 },
            stack_lost: !same_stack || self.stack_lost,
            select: meet_option(self.select, other.select),
            windows: [0, 1].map(|w| meet_option(self.windows[w], other.windows[w])),
            return_used: self.return_used || other.return_used,
            switched: [0, 1].map(|w| self.switched[w] || other.switched[w]),
        }
    }

    /// The registers after a call whose effects are unknown.
    fn forget_registers(&mut self) {
        self.a = Sym::Unknown;
        self.x = Sym::Unknown;
        self.y = Sym::Unknown;
        self.carry = None;
    }

    /// Like [`Self::apply`], but keeping what the call does not know.
    fn apply_hopeful(&mut self, summary: &Summary) {
        let mut known = summary.clone();
        known.windows = summary.windows.map(|w| w.filter(|v| *v != Sym::Unknown));
        known.select = summary.select.filter(|v| *v != Sym::Unknown);
        known.forgets_memory = false;
        known.lost.clear();
        known.changed.clear();
        self.apply(&known);
    }

    /// After a call whose effects are unknown.
    pub fn forget(&mut self) {
        self.a = Sym::Unknown;
        self.x = Sym::Unknown;
        self.y = Sym::Unknown;
        self.carry = None;
        self.clobber_all();
        self.select = Some(Sym::Unknown);
        self.windows = [Some(Sym::Unknown); 2];
    }

    /// A value of the routine `summary` describes, in terms of this state
    /// at the call.
    fn substitute(&self, value: Sym) -> Sym {
        match value {
            Sym::Input(input, add) => match input {
                Input::A => self.a,
                Input::X => self.x,
                Input::Y => self.y,
                Input::Mem(address) => self.read(u32::from(address)),
                Input::Return(_) | Input::AfterCall => Sym::Unknown,
            }
            .add(add),
            other => other,
        }
    }

    /// Applies the effects of a call to a routine that returns.
    pub fn apply(&mut self, summary: &Summary) {
        let windows = summary.windows.map(|w| w.map(|v| self.substitute(v)));
        let select = summary.select.map(|v| self.substitute(v));
        let memory: Vec<(u16, Sym)> = summary
            .memory
            .iter()
            .map(|(a, v)| (*a, self.substitute(*v)))
            .collect();
        self.a = Sym::Unknown;
        self.x = Sym::Unknown;
        self.y = Sym::Unknown;
        self.carry = None;
        if summary.forgets_memory {
            self.clobber_all();
        }
        for (low, high) in &summary.lost {
            self.clobber(u32::from(*low), u32::from(*high));
        }
        for address in &summary.changed {
            self.write(u32::from(*address), Sym::Unknown);
        }
        for (address, value) in memory {
            self.write(u32::from(address), value);
        }
        for (w, value) in windows.into_iter().enumerate() {
            if value.is_some() {
                self.windows[w] = value;
            }
        }
        if select.is_some() {
            self.select = select;
        }
    }

    /// The value an instruction stores.
    pub fn stored(&self, mnemonic: Mnemonic) -> Sym {
        match mnemonic {
            Mnemonic::Sta => self.a,
            Mnemonic::Stx => self.x,
            Mnemonic::Sty => self.y,
            _ => Sym::Unknown,
        }
    }

    /// Executes an instruction that does not change the flow (jumps,
    /// calls, returns and branches are followed by the caller).
    /// `rom_byte` reads a byte of the ROM; `mmc3` selects how writes to
    /// $8000-$9FFF switch banks.
    pub fn step(&mut self, insn: Insn, rom_byte: impl Fn(u32) -> Option<u8>, mmc3: bool) -> Effect {
        let mut effect = Effect::default();
        let direct = matches!(insn.mode, Mode::Zp | Mode::Abs);
        let address = insn.value;
        let load = |state: &Self| match insn.mode {
            Mode::Imm => Sym::Const(address as u8),
            Mode::Zp => state.read(address),
            Mode::Abs if tracked(address) => state.read(address),
            Mode::Abs => rom_byte(address).map_or(Sym::Unknown, Sym::Const),
            // Through a pointer holding the return address.
            Mode::Zi | Mode::Zix | Mode::Ziy if state.pointer_is_return(insn) => {
                Sym::Input(Input::AfterCall, 0)
            }
            _ => Sym::Unknown,
        };
        match insn.mnemonic {
            Mnemonic::Lda => self.a = load(self),
            Mnemonic::Ldx => self.x = load(self),
            Mnemonic::Ldy => self.y = load(self),
            Mnemonic::Sta | Mnemonic::Stx | Mnemonic::Sty => {
                let value = self.stored(insn.mnemonic);
                effect = self.store(insn, value, mmc3);
            }
            Mnemonic::Tax => self.x = self.a,
            Mnemonic::Tay => self.y = self.a,
            Mnemonic::Txa => self.a = self.x,
            Mnemonic::Tya => self.a = self.y,
            Mnemonic::Tsx => self.x = Sym::Unknown,
            Mnemonic::Txs => {
                self.stack.clear();
                self.stack_lost = true;
            }
            Mnemonic::Inx => self.x = self.x.add(1),
            Mnemonic::Iny => self.y = self.y.add(1),
            Mnemonic::Dex => self.x = self.x.add(0xff),
            Mnemonic::Dey => self.y = self.y.add(0xff),
            Mnemonic::Inc | Mnemonic::Dec => {
                let add = if insn.mnemonic == Mnemonic::Inc {
                    1
                } else {
                    0xff
                };
                let value = if direct {
                    self.read(address).add(add)
                } else {
                    Sym::Unknown
                };
                effect = self.store(insn, value, mmc3);
                effect.stored = None;
            }
            Mnemonic::Clc => self.carry = Some(false),
            Mnemonic::Sec => self.carry = Some(true),
            Mnemonic::Adc | Mnemonic::Sbc => {
                let operand = load(self).constant();
                let (a, carry) = match (self.a, operand, self.carry) {
                    (Sym::Const(a), Some(n), Some(c)) => {
                        let n = if insn.mnemonic == Mnemonic::Sbc {
                            !n
                        } else {
                            n
                        };
                        let sum = u16::from(a) + u16::from(n) + u16::from(c);
                        (Sym::Const(sum as u8), Some(sum > 0xff))
                    }
                    (Sym::Input(..), Some(n), Some(c)) => {
                        let n = if insn.mnemonic == Mnemonic::Sbc {
                            !n
                        } else {
                            n
                        };
                        (self.a.add(n.wrapping_add(u8::from(c))), None)
                    }
                    _ => (Sym::Unknown, None),
                };
                self.a = a;
                self.carry = carry;
            }
            Mnemonic::And | Mnemonic::Ora | Mnemonic::Eor => {
                self.a = match (self.a, load(self)) {
                    (Sym::Const(a), Sym::Const(n)) => Sym::Const(match insn.mnemonic {
                        Mnemonic::And => a & n,
                        Mnemonic::Ora => a | n,
                        _ => a ^ n,
                    }),
                    _ => Sym::Unknown,
                };
            }
            Mnemonic::Asl | Mnemonic::Lsr | Mnemonic::Rol | Mnemonic::Ror => {
                if insn.mode == Mode::Acc {
                    let (a, carry) = match (self.a, self.carry) {
                        (Sym::Const(a), carry) => shift(insn.mnemonic, a, carry),
                        _ => (None, None),
                    };
                    self.a = a.map_or(Sym::Unknown, Sym::Const);
                    self.carry = carry;
                } else {
                    effect = self.store(insn, Sym::Unknown, mmc3);
                    effect.stored = None;
                    self.carry = None;
                }
            }
            Mnemonic::Cmp | Mnemonic::Cpx | Mnemonic::Cpy | Mnemonic::Bit | Mnemonic::Plp => {
                self.carry = None;
                if insn.mnemonic == Mnemonic::Plp {
                    self.pull();
                }
            }
            Mnemonic::Pha => self.push(self.a),
            Mnemonic::Php => self.push(Sym::Unknown),
            Mnemonic::Pla => self.a = self.pull(),
            _ => {}
        }
        effect
    }

    /// A write of `value` by `insn`: to RAM (all the RAM it may reach when
    /// the address is not known) or to the mapper.
    fn store(&mut self, insn: Insn, value: Sym, mmc3: bool) -> Effect {
        let mut effect = Effect::default();
        let base = insn.value;
        let index = match insn.mode {
            Mode::Zpx | Mode::Ax => Some(self.x),
            Mode::Zpy | Mode::Ay => Some(self.y),
            _ => None,
        };
        // A pointer through which the instruction writes, when known.
        let pointer = self.pointer(insn).and_then(|p| self.word(p)).map(u32::from);
        let address = match (insn.mode, index.and_then(Sym::constant)) {
            (Mode::Zp | Mode::Abs, _) => Some(base),
            (Mode::Zpx | Mode::Zpy, Some(i)) => Some((base + u32::from(i)) & 0xff),
            (Mode::Ax | Mode::Ay, Some(i)) => Some((base + u32::from(i)) & 0xffff),
            (Mode::Zi | Mode::Zix, _) => pointer,
            (Mode::Ziy, _) => pointer
                .zip(self.y.constant())
                .map(|(p, y)| (p + u32::from(y)) & 0xffff),
            _ => None,
        };
        match (address, insn.mode) {
            (Some(address), _) if tracked(address) => {
                self.write(address, value);
                if value.constant().is_some() {
                    effect.stored = Some(address as u16);
                }
            }
            (Some(address), _) if address >= 0x8000 => {
                if mmc3 {
                    effect.mapper = Some(self.mmc3_write(Some(address), value));
                }
            }
            (Some(_), _) => {}
            (None, Mode::Zpx | Mode::Zpy) => self.clobber(0, 0xff),
            (None, Mode::Ax | Mode::Ay | Mode::Ziy)
                if insn.mode != Mode::Ziy || pointer.is_some() =>
            {
                // From the base (or the pointer) up to 255 bytes on, wrapping
                // around $FFFF.
                let low = if insn.mode == Mode::Ziy {
                    pointer.unwrap_or(base)
                } else {
                    base
                };
                let high = low + 0xff;
                if low < 0x8000 {
                    self.clobber(low, high.min(0x7fff));
                }
                if high > 0xffff {
                    self.clobber(0, high & 0xffff);
                }
                if mmc3 && high >= 0x8000 && low <= 0x9fff {
                    effect.mapper = Some(self.mmc3_write(None, value));
                }
            }
            // Through a pointer that is not known: any RAM, and the mapper.
            _ => {
                self.clobber_all();
                if mmc3 {
                    self.select = Some(Sym::Unknown);
                    self.windows = [Some(Sym::Unknown); 2];
                }
            }
        }
        effect
    }

    /// The zero page pointer an indirect instruction uses, when known.
    fn pointer(&self, insn: Insn) -> Option<u32> {
        match insn.mode {
            Mode::Zi | Mode::Ziy => Some(insn.value & 0xff),
            Mode::Zix => self
                .x
                .constant()
                .map(|x| (insn.value + u32::from(x)) & 0xff),
            _ => None,
        }
    }

    /// Whether the pointer an indirect instruction uses holds the return
    /// address (both bytes pulled from below the routine's pushes).
    fn pointer_is_return(&self, insn: Insn) -> bool {
        self.pointer(insn)
            .is_some_and(|p| self.read(p).is_return() && self.read((p + 1) & 0xff).is_return())
    }

    /// An MMC3 write to $8000-$9FFF (at `address` if it is known): bank
    /// select (even) or bank data (odd). Registers 6 and 7 hold the PRG banks
    /// at $8000 and $A000 (PRG mode 0; mode 1 is not followed).
    fn mmc3_write(&mut self, address: Option<u32>, value: Sym) -> String {
        let Some(address) = address else {
            self.select = Some(Sym::Unknown);
            self.windows = [Some(Sym::Unknown); 2];
            self.switched = [true; 2];
            return "an indexed write to the mapper".into();
        };
        let register = match (address >> 13, address % 2) {
            (5, 0) => Some("mirroring"),
            (5, _) => Some("PRG RAM protect"),
            (6, 0) => Some("IRQ latch"),
            (6, _) => Some("IRQ reload"),
            (7, 0) => Some("IRQ disable"),
            (7, _) => Some("IRQ enable"),
            _ => None,
        };
        if let Some(register) = register {
            return format!("MMC3 {register}");
        }
        let shown = |v: Sym| {
            v.constant()
                .map_or("an unknown value".into(), |v| format!("${v:02X}"))
        };
        if address % 2 == 0 {
            self.select = Some(value);
            // PRG mode 1 (bit 6) puts R6 at $C000 and a fixed bank at
            // $8000: the bank there is no longer known.
            if value.constant().is_none_or(|v| v & 0x40 != 0) {
                self.windows[0] = Some(Sym::Unknown);
                self.switched[0] = true;
            }
            return match value.constant() {
                Some(v) if v & 7 >= 6 && v & 0x40 != 0 => format!(
                    "MMC3 bank select ${v:02X}: R{} in PRG mode 1, not followed",
                    v & 7
                ),
                Some(v) if v & 7 >= 6 => format!(
                    "MMC3 bank select ${v:02X}: R{} (PRG bank at {})",
                    v & 7,
                    if v & 7 == 6 { "$8000" } else { "$A000" }
                ),
                Some(v) => format!("MMC3 bank select ${v:02X}: R{} (CHR)", v & 7),
                None => format!("MMC3 bank select with {}", shown(value)),
            };
        }
        match self.select.and_then(Sym::constant) {
            Some(s) if s & 7 == 6 && s & 0x40 == 0 => {
                self.windows[0] = Some(value);
                self.switched[0] = true;
                format!("MMC3 R6 = {}: the PRG bank at $8000", shown(value))
            }
            Some(s) if s & 7 == 7 => {
                self.windows[1] = Some(value);
                self.switched[1] = true;
                format!("MMC3 R7 = {}: the PRG bank at $A000", shown(value))
            }
            Some(s) if s & 7 == 6 => {
                self.windows[0] = Some(Sym::Unknown);
                self.switched[0] = true;
                format!("MMC3 R6 = {} in PRG mode 1, not followed", shown(value))
            }
            Some(s) => format!("MMC3 R{} = {} (CHR)", s & 7, shown(value)),
            None => {
                self.windows = [Some(Sym::Unknown); 2];
                self.switched = [true; 2];
                "MMC3 bank data after an unknown bank select".into()
            }
        }
    }
}

fn shift(mnemonic: Mnemonic, a: u8, carry: Option<bool>) -> (Option<u8>, Option<bool>) {
    match mnemonic {
        Mnemonic::Asl => (Some(a << 1), Some(a & 0x80 != 0)),
        Mnemonic::Lsr => (Some(a >> 1), Some(a & 1 != 0)),
        Mnemonic::Rol => (carry.map(|c| a << 1 | u8::from(c)), Some(a & 0x80 != 0)),
        _ => (carry.map(|c| a >> 1 | u8::from(c) << 7), Some(a & 1 != 0)),
    }
}

/// What a call to a routine does, in terms of the state at the call.
#[derive(Clone, Debug, Default)]
pub(crate) struct Summary {
    /// The banks at $8000 and $A000 on return; `None`: unchanged.
    windows: [Option<Sym>; 2],
    select: Option<Sym>,
    /// RAM written the same way on every return.
    memory: Vec<(u16, Sym)>,
    /// Other RAM written.
    changed: Vec<u16>,
    /// RAM ranges written in unknown ways.
    lost: Vec<(u16, u16)>,
    /// RAM the routine may change without being followed.
    forgets_memory: bool,
    /// Some path returns to the caller.
    pub returns: bool,
    /// A path that can be followed returns to the caller.
    pub returns_known: bool,
    /// Some path keeps its return address: it reads arguments after the
    /// JSR, or a jump table there.
    pub takes_return: bool,
    /// Some path jumps through a pointer after taking its return address:
    /// a jump table follows each JSR.
    pub dispatch: bool,
    /// The banks after a call are not known, but every path that can be
    /// followed switches in the banks held in these RAM addresses ($8000,
    /// $A000): the annotation `switch_banks` would say so.
    pub hint: Option<[Option<u16>; 2]>,
}

impl Summary {
    /// A routine that switches in the banks held in RAM (`switch_banks`).
    fn switching(banks: [Option<u16>; 2]) -> Self {
        Self {
            windows: banks.map(|b| b.map(|a| Sym::Input(Input::Mem(a), 0))),
            // The routine leaves another register selected.
            select: Some(Sym::Unknown),
            returns: true,
            returns_known: true,
            ..Self::default()
        }
    }

    fn unknown() -> Self {
        Self {
            windows: [Some(Sym::Unknown); 2],
            select: Some(Sym::Unknown),
            forgets_memory: true,
            returns: true,
            ..Self::default()
        }
    }
}

/// The code a summary follows.
pub(crate) trait Code {
    /// The NMOS 6502 instruction at `loc`, whose bytes are in the ROM.
    fn insn(&self, loc: Loc) -> Option<Insn>;
    /// Where code continues `n` bytes after `loc`.
    fn next(&self, loc: Loc, n: usize) -> Option<Loc>;
    /// The location of `address` referred to from `from` with the banks
    /// `windows` switched in, if one bank holds it.
    fn target(&self, from: Loc, address: u32, windows: [Option<u8>; 2]) -> Option<Loc>;
    /// The ROM byte at `address` seen from `from`, if one bank holds it.
    fn rom_byte(&self, from: Loc, address: u32, windows: [Option<u8>; 2]) -> Option<u8>;
    /// `no_return` and `inline_bytes` of an annotated entry point.
    fn annotated(&self, loc: Loc) -> Option<(bool, usize)>;
    /// `switch_banks` of an annotated entry point.
    fn switches(&self, loc: Loc) -> Option<[Option<u16>; 2]>;
    /// The window ($8000 = 0, $A000 = 1) of a bank that is switched.
    fn window_of(&self, bank: usize) -> Option<usize>;
    /// The PRG bank a mapper bank number selects.
    fn bank_number(&self, bank: u8) -> usize;
    fn mmc3(&self) -> bool;
}

type Key = (Loc, [Option<u8>; 2]);

/// Summaries of the routines called, by entry and the banks switched in at
/// the call.
#[derive(Default)]
pub(crate) struct Summaries {
    memo: HashMap<Key, Arc<Summary>>,
    scans: HashMap<Key, bool>,
    active: Vec<Key>,
}

impl Summaries {
    /// What a call to `entry` with the banks `windows` switched in does.
    pub fn get(&mut self, code: &impl Code, entry: Loc, windows: [Option<u8>; 2]) -> Arc<Summary> {
        let mut windows = windows;
        if let Some(w) = code.window_of(entry.bank) {
            windows[w] = Some(entry.bank as u8);
        }
        if let Some(banks) = code.switches(entry) {
            return Arc::new(Summary::switching(banks));
        }
        let key = (entry, windows);
        if let Some(summary) = self.memo.get(&key) {
            return summary.clone();
        }
        if self.active.len() >= MAX_DEPTH || self.active.contains(&key) {
            return Arc::new(Summary::unknown());
        }
        self.active.push(key);
        let mut summary = self
            .explore(code, entry, windows, false)
            .unwrap_or_else(|| self.fallback(code, key));
        // Advice only: the banks the routine switches in if the code it
        // cannot follow changed nothing.
        if summary.windows.contains(&Some(Sym::Unknown))
            && let Some(hopeful) = self.explore(code, entry, windows, true)
        {
            let hint = hopeful.windows.map(|w| match w {
                Some(Sym::Input(Input::Mem(a), 0)) => Some(a),
                _ => None,
            });
            summary.hint = (hint != [None; 2]).then_some(hint);
        }
        self.active.pop();
        let summary = Arc::new(summary);
        self.memo.insert(key, summary.clone());
        summary
    }

    /// Follows every path of the routine; `None` if there are too many.
    /// `hopeful`: as if calls and jumps that cannot be followed changed
    /// nothing but the registers (for advice only).
    fn explore(
        &mut self,
        code: &impl Code,
        entry: Loc,
        windows: [Option<u8>; 2],
        hopeful: bool,
    ) -> Option<Summary> {
        let current = |state: &State| -> [Option<u8>; 2] {
            [0, 1].map(|w| match state.windows[w] {
                Some(v) => v.constant(),
                None => windows[w],
            })
        };
        let mut steps = 0;
        let mut exits: Vec<State> = Vec::new();
        let mut unknown_exit = false;
        let mut summary = Summary::default();
        // What is known at each instruction reached: paths that join (or
        // loop back) are walked again knowing only what they share, until
        // they know nothing new.
        let mut seen: HashMap<Loc, State> = HashMap::new();
        let mut paths = vec![(entry, State::routine([None; 2]))];
        while let Some((mut loc, mut state)) = paths.pop() {
            loop {
                steps += 1;
                if steps > MAX_SUMMARY_STEPS {
                    return None;
                }
                if let Some(before) = seen.get(&loc) {
                    let met = state.meet(before);
                    if met == *before {
                        break;
                    }
                    state = met;
                }
                seen.insert(loc, state.clone());
                // Code that switched another bank (or an unknown one) into
                // the window it runs in goes on elsewhere.
                if let Some(w) = code.window_of(loc.bank) {
                    let bank = current(&state)[w];
                    if bank.is_some_and(|b| code.bank_number(b) != loc.bank)
                        || (state.switched[w] && bank.is_none())
                    {
                        unknown_exit = true;
                        break;
                    }
                }
                // Code that goes on into an annotated routine returns as it does.
                if loc != entry
                    && let Some(banks) = code.switches(loc)
                {
                    state.apply(&Summary::switching(banks));
                    exits.push(state);
                    break;
                }
                let Some(insn) = code.insn(loc) else {
                    unknown_exit = true;
                    break;
                };
                let mut advance = insn.len;
                match (insn.mnemonic, insn.mode) {
                    (Mnemonic::Jsr, _) => {
                        let target = code.target(loc, insn.value, current(&state));
                        let annotated = target.and_then(|t| code.annotated(t));
                        if annotated.is_some_and(|(no_return, _)| no_return) {
                            break;
                        }
                        match target {
                            Some(target) => {
                                let callee = self.get(code, target, current(&state));
                                if annotated.is_some() {
                                    // Arguments after the JSR: the routine
                                    // returns past them.
                                    state.forget();
                                } else if callee.takes_return || !callee.returns {
                                    break;
                                } else if hopeful {
                                    state.apply_hopeful(&callee);
                                } else {
                                    state.apply(&callee);
                                }
                            }
                            None if hopeful => state.forget_registers(),
                            None => state.forget(),
                        }
                        advance += annotated.map_or(0, |(_, inline)| inline);
                    }
                    (Mnemonic::Jmp, Mode::Abs) => {
                        match code.target(loc, insn.value, current(&state)) {
                            Some(target) => {
                                loc = target;
                                continue;
                            }
                            None => {
                                unknown_exit = true;
                                break;
                            }
                        }
                    }
                    (Mnemonic::Jmp, _) => {
                        // A jump table after the JSR: the pointer was read
                        // through the return address.
                        let (low, high) = State::jump_bytes(insn.value);
                        let after_call = [low, high].iter().any(|a| state.read(*a).is_after_call());
                        if state.return_used {
                            summary.takes_return = true;
                            summary.dispatch |= after_call;
                        } else {
                            unknown_exit = true;
                        }
                        break;
                    }
                    (Mnemonic::Rts, _) => {
                        if state.return_used {
                            summary.takes_return = true;
                        } else if state.stack_lost || !state.stack.is_empty() {
                            // An RTS jump: it goes on elsewhere.
                            unknown_exit = true;
                        } else if state.popped == 0 {
                            exits.push(state);
                        }
                        // Otherwise it returns to the caller's caller.
                        break;
                    }
                    (Mnemonic::Rti | Mnemonic::Brk, _) => {
                        unknown_exit = true;
                        break;
                    }
                    (_, Mode::Rel) => match code.target(loc, insn.value, current(&state)) {
                        Some(target) => paths.push((target, state.clone())),
                        None => unknown_exit = true,
                    },
                    _ => {
                        let at = current(&state);
                        state.step(insn, |a| code.rom_byte(loc, a, at), code.mmc3());
                    }
                }
                match code.next(loc, advance) {
                    Some(next) => loc = next,
                    None => {
                        unknown_exit = true;
                        break;
                    }
                }
            }
        }
        summary.returns = unknown_exit || !exits.is_empty();
        summary.returns_known = !exits.is_empty();
        if unknown_exit && !hopeful {
            let unknown = Summary::unknown();
            summary.windows = unknown.windows;
            summary.select = unknown.select;
            summary.forgets_memory = true;
            return Some(summary);
        }
        let Some(first) = exits.first() else {
            return Some(summary);
        };
        summary.windows = [0, 1].map(|w| {
            exits
                .iter()
                .fold(first.windows[w], |acc, e| meet_option(acc, e.windows[w]))
        });
        summary.select = exits
            .iter()
            .fold(first.select, |acc, e| meet_option(acc, e.select));
        summary.forgets_memory = exits.iter().any(|e| !e.entry_memory);
        for exit in &exits {
            for range in &exit.lost {
                if !summary.lost.contains(range) {
                    summary.lost.push(*range);
                }
            }
        }
        let mut written: Vec<u16> = exits
            .iter()
            .flat_map(|e| e.memory.iter().map(|(a, _)| *a))
            .collect();
        written.sort_unstable();
        written.dedup();
        for address in written {
            let value = |e: &State| {
                e.memory
                    .iter()
                    .find(|(a, _)| *a == address)
                    .map(|(_, v)| *v)
            };
            let first_value = value(first);
            match first_value {
                Some(v) if exits.iter().all(|e| value(e) == Some(v)) && v != Sym::Unknown => {
                    summary.memory.push((address, v));
                }
                _ => summary.changed.push(address),
            }
        }
        Some(summary)
    }

    /// For a routine with too many paths: it leaves the banks alone if no
    /// code it reaches writes to the mapper or jumps where the scan cannot
    /// follow.
    fn fallback(&mut self, code: &impl Code, key: Key) -> Summary {
        if self.mapper_free(code, key) {
            Summary {
                forgets_memory: true,
                returns: true,
                ..Summary::default()
            }
        } else {
            Summary::unknown()
        }
    }

    fn mapper_free(&mut self, code: &impl Code, key: Key) -> bool {
        if let Some(free) = self.scans.get(&key) {
            return *free;
        }
        let (entry, windows) = key;
        let mut seen = HashSet::new();
        let mut work = vec![entry];
        let mut free = true;
        while let Some(loc) = work.pop() {
            if !seen.insert(loc) {
                continue;
            }
            if seen.len() > MAX_SCAN {
                free = false;
                break;
            }
            let Some(insn) = code.insn(loc) else {
                free = false;
                break;
            };
            let writes = matches!(
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
            );
            let absolute = matches!(insn.mode, Mode::Abs | Mode::Ax | Mode::Ay);
            let indirect = matches!(insn.mode, Mode::Zi | Mode::Zix | Mode::Ziy);
            if writes && ((absolute && insn.value + 0xff >= 0x8000) || indirect) {
                free = false;
                break;
            }
            let mut follow = |address: u32| match code.target(loc, address, windows) {
                Some(target) => {
                    work.push(target);
                    true
                }
                None => false,
            };
            let continues = match (insn.mnemonic, insn.mode) {
                (Mnemonic::Jsr, _) | (_, Mode::Rel) => {
                    if !follow(insn.value) {
                        free = false;
                        break;
                    }
                    true
                }
                (Mnemonic::Jmp, Mode::Abs) => {
                    if !follow(insn.value) {
                        free = false;
                        break;
                    }
                    false
                }
                (Mnemonic::Jmp | Mnemonic::Rti | Mnemonic::Brk, _) => {
                    free = false;
                    break;
                }
                (Mnemonic::Rts, _) => false,
                _ => true,
            };
            if continues {
                match code.next(loc, insn.len) {
                    Some(next) => work.push(next),
                    None => {
                        free = false;
                        break;
                    }
                }
            }
        }
        self.scans.insert(key, free);
        free
    }
}

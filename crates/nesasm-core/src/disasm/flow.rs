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

    fn meet(self, other: Self) -> Self {
        if self == other { self } else { Self::Unknown }
    }
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
            None if self.entry_memory => Sym::Input(Input::Mem(address), 0),
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
            self.memory.remove(0);
        }
        self.memory.push((address, value));
        if value.is_return() {
            self.return_used = true;
        }
    }

    /// The known value of the 16-bit pointer at `address`.
    pub fn word(&self, address: u32) -> Option<u16> {
        let low = self.read(address).constant()?;
        let high = self.read(address + 1).constant()?;
        Some(u16::from_le_bytes([low, high]))
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
        let memory = self
            .memory
            .iter()
            .filter(|(a, v)| other.memory.iter().any(|(b, w)| a == b && v == w))
            .copied()
            .collect();
        let same_stack = self.stack == other.stack
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
            memory,
            entry_memory: self.entry_memory && other.entry_memory,
            stack: if same_stack {
                self.stack.clone()
            } else {
                Vec::new()
            },
            popped: if same_stack { self.popped } else { 0 },
            stack_lost: !same_stack || self.stack_lost,
            select: if self.select == other.select {
                self.select
            } else {
                None
            },
            windows: [0, 1].map(|w| {
                if self.windows[w] == other.windows[w] {
                    self.windows[w]
                } else {
                    None
                }
            }),
            return_used: self.return_used || other.return_used,
        }
    }

    /// After a call whose effects are unknown.
    pub fn forget(&mut self) {
        self.a = Sym::Unknown;
        self.x = Sym::Unknown;
        self.y = Sym::Unknown;
        self.carry = None;
        self.memory.clear();
        self.entry_memory = false;
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
                Input::Return(_) => Sym::Unknown,
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
            self.memory.clear();
            self.entry_memory = false;
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
            _ => Sym::Unknown,
        };
        match insn.mnemonic {
            Mnemonic::Lda => self.a = load(self),
            Mnemonic::Ldx => self.x = load(self),
            Mnemonic::Ldy => self.y = load(self),
            Mnemonic::Sta | Mnemonic::Stx | Mnemonic::Sty => {
                let value = self.stored(insn.mnemonic);
                if direct && tracked(address) {
                    self.write(address, value);
                    if value.constant().is_some() {
                        effect.stored = Some(address as u16);
                    }
                } else if mmc3 && address >= 0x8000 {
                    effect.mapper = Some(self.mmc3_write(direct.then_some(address), value));
                }
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
            Mnemonic::Inc | Mnemonic::Dec if direct => {
                let add = if insn.mnemonic == Mnemonic::Inc {
                    1
                } else {
                    0xff
                };
                let value = self.read(address).add(add);
                self.write(address, value);
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
                    if direct {
                        self.write(address, Sym::Unknown);
                    }
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

    /// An MMC3 write to $8000-$9FFF (at `address` if it is known): bank
    /// select (even) or bank data (odd). Registers 6 and 7 hold the PRG banks
    /// at $8000 and $A000 (PRG mode 0; mode 1 is not followed).
    fn mmc3_write(&mut self, address: Option<u32>, value: Sym) -> String {
        let Some(address) = address else {
            self.select = Some(Sym::Unknown);
            self.windows = [Some(Sym::Unknown); 2];
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
                format!("MMC3 R6 = {}: the PRG bank at $8000", shown(value))
            }
            Some(s) if s & 7 == 7 => {
                self.windows[1] = Some(value);
                format!("MMC3 R7 = {}: the PRG bank at $A000", shown(value))
            }
            Some(s) if s & 7 == 6 => {
                self.windows[0] = Some(Sym::Unknown);
                format!("MMC3 R6 = {} in PRG mode 1, not followed", shown(value))
            }
            Some(s) => format!("MMC3 R{} = {} (CHR)", s & 7, shown(value)),
            None => {
                self.windows = [Some(Sym::Unknown); 2];
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
    /// RAM the routine may change without being followed.
    forgets_memory: bool,
    /// Some path returns to the caller.
    pub returns: bool,
    /// Some path keeps its return address: it reads arguments after the
    /// JSR, or a jump table there.
    pub takes_return: bool,
    /// Some path jumps through a pointer after taking its return address:
    /// a jump table follows each JSR.
    pub dispatch: bool,
}

impl Summary {
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
    /// The window ($8000 = 0, $A000 = 1) of a bank that is switched.
    fn window_of(&self, bank: usize) -> Option<usize>;
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
        let key = (entry, windows);
        if let Some(summary) = self.memo.get(&key) {
            return summary.clone();
        }
        if self.active.len() >= MAX_DEPTH || self.active.contains(&key) {
            return Arc::new(Summary::unknown());
        }
        self.active.push(key);
        let summary = self
            .explore(code, entry, windows)
            .unwrap_or_else(|| self.fallback(code, key));
        self.active.pop();
        let summary = Arc::new(summary);
        self.memo.insert(key, summary.clone());
        summary
    }

    /// Follows every path of the routine; `None` if there are too many.
    fn explore(
        &mut self,
        code: &impl Code,
        entry: Loc,
        windows: [Option<u8>; 2],
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
        let mut paths = vec![(entry, State::routine([None; 2]), HashSet::new())];
        while let Some((mut loc, mut state, mut seen)) = paths.pop() {
            loop {
                steps += 1;
                if steps > MAX_SUMMARY_STEPS {
                    return None;
                }
                // A loop: the first time through stands for the others.
                if !seen.insert(loc) {
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
                                } else {
                                    state.apply(&callee);
                                }
                            }
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
                        if state.return_used {
                            summary.dispatch = true;
                            summary.takes_return = true;
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
                        Some(target) => paths.push((target, state.clone(), seen.clone())),
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
        if unknown_exit {
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
            if writes && absolute && insn.value >= 0x8000 {
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

//! 6502 instruction encoding: operand syntax, addressing-mode selection and
//! the legacy extensions (low_byte/high_byte, `.l`/`.h`, `[zp].tag`, `,x++`).

use super::Engine;
use crate::error::AsmResult;
use crate::opcode::{Mnemonic, Mode};
use crate::source;

/// Byte selected by `low_byte`/`.l` or `high_byte`/`.h`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ByteSelect {
    Low,
    High,
}

impl Engine<'_> {
    /// Encodes `op operand`. `op` is the upper-case name, possibly with a
    /// `.L`/`.H` extension; unknown names fail when the opcode is looked up,
    /// after the operand has been checked.
    pub(super) fn instruction(&self, op: &str, operand: &str) -> AsmResult<Vec<u8>> {
        let (name, extension) = op.split_once('.').map_or((op, None), |(n, e)| (n, Some(e)));
        let mnemonic = Mnemonic::parse(name);
        let opcode = |mode| mnemonic.and_then(|m| m.opcode(mode));
        // `lda.l` / `lda.h` are the C# spellings of low_byte / high_byte.
        let suffix = match extension {
            None => None,
            Some("L") => Some(ByteSelect::Low),
            Some("H") => Some(ByteSelect::High),
            Some(_) => return Err("Unknown instruction extension; use .l or .h".into()),
        };
        if suffix.is_some() && (opcode(Mode::Imp).is_some() || opcode(Mode::Rel).is_some()) {
            return Err("Instruction extension not supported".into());
        }
        if let Some(code) = opcode(Mode::Imp) {
            if !operand.is_empty() {
                return Err("Unexpected operand".into());
            }
            return Ok(vec![code]);
        }
        if let Some(code) = opcode(Mode::Rel) {
            let value = self.value(operand)?;
            let delta = value.wrapping_sub(self.pc() + 2) as i32;
            if self.pass.is_emitting() && !(-128..=127).contains(&delta) {
                return Err("Branch address out of range".into());
            }
            return Ok(vec![code, delta as u8]);
        }
        let mut compact = operand.trim();
        let prefix = if compact.to_ascii_lowercase().starts_with("low_byte ") {
            compact = compact[9..].trim_start();
            Some(ByteSelect::Low)
        } else if compact.to_ascii_lowercase().starts_with("high_byte ") {
            compact = compact[10..].trim_start();
            Some(ByteSelect::High)
        } else {
            None
        };
        if suffix.is_some() && prefix.is_some() {
            return Err("Duplicate instruction extension".into());
        }
        let select = suffix.or(prefix);
        if compact.eq_ignore_ascii_case("A") {
            return opcode(Mode::Acc)
                .map(|c| vec![c])
                .ok_or_else(|| "Invalid accumulator mode".into());
        }
        let operand = self.operand(compact, name == "JMP")?;
        let Operand {
            mut expr,
            mut mode,
            auto_increment,
            auto_tag,
            parenthesized,
        } = operand;
        // In immediate mode `<`/`>` are the unary low/high byte operators.
        let immediate = mode == Mode::Imm;
        let forced = !immediate && expr.trim().starts_with('<');
        let absolute = !immediate && expr.trim().starts_with('>');
        expr = expr.trim();
        if !immediate {
            expr = expr.trim_start_matches(['<', '>']);
        }
        let mut value = self.value(expr)?;
        // C#-compatible fallback: AUTOZP `(addr),Y` above zero page is absolute,Y.
        if parenthesized && mode == Mode::Ziy && value > 255 {
            mode = Mode::Ay;
        }
        if forced || (self.auto_zp && !absolute && value <= 255) {
            let zp = match mode {
                Mode::Abs => Mode::Zp,
                Mode::Ax => Mode::Zpx,
                Mode::Ay => Mode::Zpy,
                m => m,
            };
            if opcode(zp).is_some() {
                mode = zp;
            } else if forced {
                return Err("Invalid zero page mode".into());
            }
        }
        let code =
            opcode(mode).ok_or_else(|| format!("Unknown instruction or addressing mode '{op}'"))?;
        let wide = matches!(mode, Mode::Abs | Mode::Ax | Mode::Ay | Mode::Ind | Mode::Ix);
        if let Some(select) = select {
            if mode == Mode::Imm {
                value = match select {
                    ByteSelect::Low => value & 255,
                    ByteSelect::High => (value >> 8) & 255,
                };
            } else if auto_increment.is_none() {
                if matches!(
                    mode,
                    Mode::Zi | Mode::Zix | Mode::Ziy | Mode::Ind | Mode::Ix
                ) {
                    return Err("Instruction extension not supported in indirect modes".into());
                }
                if select == ByteSelect::High {
                    value = value.wrapping_add(1);
                }
            }
        }
        let too_large = if wide {
            value > 65535
        } else if mode == Mode::Imm {
            value > 255 && value < 0xffff_ff00
        } else {
            value > 255
        };
        if self.pass.is_emitting() && too_large {
            return Err("Operand size error".into());
        }
        let mut bytes = Vec::with_capacity(6);
        if let Some(tag) = auto_tag {
            bytes.extend([0xa0, tag]);
        }
        bytes.extend([code, value as u8]);
        if wide {
            bytes.push((value >> 8) as u8);
        }
        if let Some(inc) = auto_increment {
            bytes.push(inc);
        }
        Ok(bytes)
    }

    /// Splits an operand into its address expression and addressing mode.
    fn operand<'t>(&self, compact: &'t str, jmp: bool) -> AsmResult<Operand<'t>> {
        let mut operand = Operand {
            expr: compact,
            mode: Mode::Abs,
            auto_increment: None,
            auto_tag: None,
            parenthesized: false,
        };
        // Parenthesized indirect forms exist only with AUTOZP, as in the C# version:
        // `(zp,X)` and `(zp),Y`. Anything else in parentheses is an expression.
        if let Some(after_paren) = compact.strip_prefix('(') {
            if !self.auto_zp {
                if after_paren.trim_start().starts_with('<') {
                    return Err(
                        "Use [..] for indirect addressing, or enable AUTOZP for (..)".into(),
                    );
                }
            } else if let Some((inner, tail)) = paren_indirect(compact) {
                operand.parenthesized = true;
                if tail.is_empty() {
                    operand.expr = &inner[..inner.rfind(',').ok_or("Invalid indirect operand")?];
                    operand.mode = if jmp { Mode::Ix } else { Mode::Zix };
                } else {
                    operand.expr = inner;
                    operand.mode = Mode::Ziy;
                    if tail == ",Y++" {
                        operand.auto_increment = Some(0xc8);
                    }
                }
                return Ok(operand);
            }
        }
        if let Some(rest) = compact.strip_prefix('#') {
            operand.expr = rest;
            operand.mode = Mode::Imm;
        } else if compact.starts_with('[') {
            self.bracket_operand(compact, jmp, &mut operand)?;
        } else {
            let upper = compact.to_ascii_uppercase().replace(' ', "");
            if [",X", ",Y", ",X++", ",Y++"]
                .iter()
                .any(|suffix| upper.ends_with(suffix))
            {
                let comma = compact.rfind(',').ok_or("Invalid indexed operand")?;
                operand.expr = compact[..comma].trim();
                let suffix = compact[comma + 1..].trim().to_ascii_uppercase();
                operand.mode = if suffix.starts_with('X') {
                    Mode::Ax
                } else {
                    Mode::Ay
                };
                if suffix.ends_with('+') {
                    operand.auto_increment =
                        Some(if operand.mode == Mode::Ax { 0xe8 } else { 0xc8 });
                }
            }
        }
        Ok(operand)
    }

    /// `[zp,x]`, `[zp],y`, `[zp],y++`, `[zp].tag`, `[zp]` and `[abs]` for JMP.
    fn bracket_operand<'t>(
        &self,
        compact: &'t str,
        jmp: bool,
        operand: &mut Operand<'t>,
    ) -> AsmResult<()> {
        let index = compact.rfind(']').ok_or("Missing indirect delimiter")?;
        let inner = &compact[1..index];
        let tail = compact[index + 1..].replace(' ', "");
        let parts = source::arguments(inner)?;
        if parts.len() == 2 && parts[1].eq_ignore_ascii_case("X") {
            operand.expr = &inner[..inner.rfind(',').ok_or("Invalid indirect operand")?];
            operand.mode = if jmp { Mode::Ix } else { Mode::Zix };
        } else if let Some(tag) = tail.strip_prefix('.') {
            operand.expr = inner;
            operand.mode = Mode::Ziy;
            let tag = self.value(tag)?;
            if self.pass.is_emitting() && tag > 255 {
                return Err("Indirect tag out of range".into());
            }
            operand.auto_tag = Some(tag as u8);
        } else if tail.to_ascii_uppercase().starts_with(",Y") {
            operand.expr = inner;
            operand.mode = Mode::Ziy;
            match tail.to_ascii_uppercase().as_str() {
                ",Y" => {}
                ",Y++" => operand.auto_increment = Some(0xc8),
                _ => return Err("Invalid indirect operand".into()),
            }
        } else if !tail.is_empty() {
            return Err("Invalid indirect operand".into());
        } else {
            operand.expr = inner;
            operand.mode = if jmp { Mode::Ind } else { Mode::Zi };
        }
        Ok(())
    }
}

struct Operand<'t> {
    expr: &'t str,
    mode: Mode,
    /// INX/INY appended by `,x++` / `,y++`.
    auto_increment: Option<u8>,
    /// `LDY #tag` prepended by `[zp].tag`.
    auto_tag: Option<u8>,
    /// An AUTOZP `(..)` indirect form.
    parenthesized: bool,
}

/// With AUTOZP, the inner expression and tail of `(zp,X)` or `(zp),Y[++]`.
fn paren_indirect(compact: &str) -> Option<(&str, String)> {
    let end = matching_paren(compact)?;
    let inner = &compact[1..end];
    let tail = compact[end + 1..].replace(' ', "").to_ascii_uppercase();
    let parts = source::arguments(inner).ok()?;
    let pre_indexed = tail.is_empty() && parts.len() == 2 && parts[1].eq_ignore_ascii_case("X");
    let post_indexed = parts.len() == 1 && (tail == ",Y" || tail == ",Y++");
    (pre_indexed || post_indexed).then_some((inner, tail))
}

/// Index of the `)` closing the operand's leading `(`, skipping quoted text.
fn matching_paren(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote = None;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

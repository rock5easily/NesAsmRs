use crate::error::{AsmError, AsmResult};
use crate::{BankRef, Region, Symbol};
use std::{cell::Cell, collections::BTreeMap};

/// Expression function calls allowed in one assembly, across both passes.
pub(crate) const FUNCTION_CALL_LIMIT: usize = 1_000_000;
const RECURSION_LIMIT: usize = 32;
const TOKEN_LIMIT: usize = 256;

/// Keywords whose argument is a symbol rather than an expression.
const SYMBOL_KEYWORDS: [&str; 6] = ["DEFINED", "BANK", "PAGE", "SIZEOF", "VRAM", "PAL"];

pub(crate) struct Context<'a> {
    pub symbols: &'a BTreeMap<String, Symbol>,
    pub regions: &'a BTreeMap<String, Region>,
    pub functions: &'a BTreeMap<String, String>,
    pub global: &'a str,
    pub pc: u32,
    pub allow_undefined: bool,
    /// Expression function calls made so far in this assembly.
    pub function_calls: &'a Cell<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Shl,
    Shr,
    Or,
    Xor,
    And,
    Complement,
    Not,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Open,
    Close,
    Comma,
}

impl Op {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "+" => Self::Add,
            "-" => Self::Sub,
            "*" => Self::Mul,
            "/" => Self::Div,
            "%" => Self::Mod,
            "<<" => Self::Shl,
            ">>" => Self::Shr,
            "|" => Self::Or,
            "^" => Self::Xor,
            "&" => Self::And,
            "~" => Self::Complement,
            "!" => Self::Not,
            "=" | "==" => Self::Eq,
            "<>" | "!=" => Self::Ne,
            "<" => Self::Lt,
            "<=" => Self::Le,
            ">" => Self::Gt,
            ">=" => Self::Ge,
            "(" => Self::Open,
            ")" => Self::Close,
            "," => Self::Comma,
            _ => return None,
        })
    }

    /// Binary operator precedence (C# table); 0 for non-binary tokens.
    const fn precedence(self) -> u8 {
        match self {
            Self::Or => 1,
            Self::Xor => 2,
            Self::And => 3,
            Self::Eq | Self::Ne => 4,
            Self::Lt | Self::Le | Self::Gt | Self::Ge => 5,
            Self::Shl | Self::Shr => 6,
            Self::Add | Self::Sub => 7,
            Self::Mul | Self::Div | Self::Mod => 8,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Token<'a> {
    Number(u32),
    Name(&'a str),
    String(&'a str),
    Op(Op),
    End,
}

pub(crate) fn evaluate(text: &str, ctx: &Context<'_>) -> AsmResult<u32> {
    evaluate_inner(text, ctx, 0)
}

fn evaluate_inner(text: &str, ctx: &Context<'_>, depth: usize) -> AsmResult<u32> {
    if depth > RECURSION_LIMIT {
        return Err(AsmError::fatal(
            "Expression function recursion limit exceeded",
        ));
    }
    let mut tokens = tokenize(text)?;
    if tokens.len() > TOKEN_LIMIT {
        return Err(AsmError::fatal("Expression complexity limit exceeded"));
    }
    tokens.push(Token::End);
    let mut parser = Parser {
        tokens,
        pos: 0,
        ctx,
        depth,
    };
    let value = parser.expr(0)?;
    if parser.peek() != Token::End {
        return Err("Unexpected expression suffix".into());
    }
    Ok(value)
}

fn tokenize(text: &str) -> AsmResult<Vec<Token<'_>>> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch == '"' || ch == '\'' {
            let body = start + 1;
            let end = loop {
                match chars.next() {
                    Some((i, c)) if c == ch => break i,
                    Some(_) => {}
                    None => return Err("Unterminated literal".into()),
                }
            };
            let value = &text[body..end];
            if ch == '\'' {
                let [byte] = value.as_bytes() else {
                    return Err("Character literal must be one byte".into());
                };
                tokens.push(Token::Number(u32::from(*byte)));
            } else {
                tokens.push(Token::String(value));
            }
        } else if ch.is_ascii_digit()
            || ch == '$'
            // `%` starts a binary literal where an operand is expected.
            || (ch == '%'
                && tokens
                    .last()
                    .is_none_or(|t| matches!(t, Token::Op(op) if *op != Op::Close)))
        {
            let mut radix = match ch {
                '$' => 16,
                '%' => 2,
                _ => 10,
            };
            let mut digits = String::new();
            if ch == '0'
                && chars
                    .peek()
                    .is_some_and(|(_, c)| c.eq_ignore_ascii_case(&'x'))
            {
                // C-style 0x1F hexadecimal.
                chars.next();
                radix = 16;
            } else if ch.is_ascii_digit() {
                digits.push(ch);
            }
            while let Some(&(_, c)) = chars.peek() {
                if c.is_digit(radix) {
                    digits.push(c);
                } else if !(c == '_' && radix == 2) {
                    // Binary literals may group digits: %1100_0011.
                    break;
                }
                chars.next();
            }
            tokens.push(Token::Number(
                u32::from_str_radix(&digits, radix).map_err(|_| "Invalid numeric literal")?,
            ));
        } else if ch.is_alphabetic() || ch == '_' || ch == '.' {
            let mut end = start + ch.len_utf8();
            while let Some(&(i, c)) = chars.peek() {
                if !(c.is_alphanumeric() || c == '_' || c == '.') {
                    break;
                }
                end = i + c.len_utf8();
                chars.next();
            }
            tokens.push(Token::Name(&text[start..end]));
        } else {
            let mut end = start + ch.len_utf8();
            if let Some(&(i, next)) = chars.peek()
                && matches!(
                    (ch, next),
                    ('<', '<' | '=' | '>') | ('>', '>' | '=') | ('!' | '=', '=')
                )
            {
                end = i + next.len_utf8();
                chars.next();
            }
            let op = Op::parse(&text[start..end])
                .ok_or_else(|| format!("Invalid expression character '{ch}'"))?;
            tokens.push(Token::Op(op));
        }
    }
    Ok(tokens)
}

struct Parser<'t, 'a, 'b> {
    tokens: Vec<Token<'t>>,
    pos: usize,
    ctx: &'a Context<'b>,
    depth: usize,
}

impl<'t, 'b> Parser<'t, '_, 'b> {
    fn peek(&self) -> Token<'t> {
        self.tokens[self.pos]
    }
    fn next(&mut self) -> Token<'t> {
        let token = self.peek();
        self.pos += 1;
        token
    }
    fn symbol(&self, name: &str) -> AsmResult<&'b Symbol> {
        let local = name.starts_with('.');
        let found = if local {
            self.ctx.symbols.get(&format!("{}{name}", self.ctx.global))
        } else {
            self.ctx.symbols.get(name)
        };
        found
            .filter(|s| local || !name.contains('.') || s.public)
            .ok_or_else(|| format!("Undefined symbol '{name}'").into())
    }
    fn expr(&mut self, min: u8) -> AsmResult<u32> {
        let mut left = match self.next() {
            Token::Number(n) => n,
            Token::Name(name) => {
                let upper = name.to_ascii_uppercase();
                if self.peek() == Token::Op(Op::Open) {
                    self.pos += 1;
                    self.function(name, true)?
                } else if upper == "HIGH" || upper == "LOW" {
                    // C# keywords are also prefix operators: `HIGH foo + 1`.
                    let n = self.expr(10)?;
                    if upper == "HIGH" {
                        (n >> 8) & 255
                    } else {
                        n & 255
                    }
                } else if SYMBOL_KEYWORDS.contains(&upper.as_str())
                    && matches!(self.peek(), Token::Name(_))
                {
                    self.function(name, false)?
                } else {
                    match self.symbol(name) {
                        Ok(s) => s.value,
                        Err(_) if self.ctx.allow_undefined => 0,
                        Err(e) => return Err(e),
                    }
                }
            }
            Token::Op(Op::Open) => {
                let n = self.expr(0)?;
                self.expect(Op::Close)?;
                n
            }
            Token::Op(Op::Mul) => self.ctx.pc,
            Token::Op(op @ (Op::Add | Op::Sub | Op::Complement | Op::Not | Op::Lt | Op::Gt)) => {
                let n = self.expr(10)?;
                match op {
                    Op::Sub => 0u32.wrapping_sub(n),
                    Op::Complement => !n,
                    Op::Not => u32::from(n == 0),
                    Op::Lt => n & 255,
                    Op::Gt => (n >> 8) & 255,
                    _ => n,
                }
            }
            _ => return Err("Expected expression".into()),
        };
        while let Token::Op(op) = self.peek() {
            let precedence = op.precedence();
            if precedence == 0 || precedence < min {
                break;
            }
            self.pos += 1;
            let right = self.expr(precedence + 1)?;
            left = self.binary(op, left, right)?;
        }
        Ok(left)
    }
    fn binary(&self, op: Op, left: u32, right: u32) -> AsmResult<u32> {
        let (signed_left, signed_right) = (left as i32, right as i32);
        Ok(match op {
            Op::Add => left.wrapping_add(right),
            Op::Sub => left.wrapping_sub(right),
            Op::Mul => left.wrapping_mul(right),
            Op::Div | Op::Mod if right == 0 => {
                if self.ctx.allow_undefined {
                    0
                } else {
                    return Err("Division by zero".into());
                }
            }
            Op::Div => signed_left
                .checked_div(signed_right)
                .ok_or("Division overflow")? as u32,
            Op::Mod => signed_left
                .checked_rem(signed_right)
                .ok_or("Division overflow")? as u32,
            Op::Shl => left.wrapping_shl(right),
            Op::Shr => signed_left.wrapping_shr(right) as u32,
            Op::Or => left | right,
            Op::Xor => left ^ right,
            Op::And => left & right,
            Op::Eq => u32::from(left == right),
            Op::Ne => u32::from(left != right),
            Op::Lt => u32::from(signed_left < signed_right),
            Op::Le => u32::from(signed_left <= signed_right),
            Op::Gt => u32::from(signed_left > signed_right),
            Op::Ge => u32::from(signed_left >= signed_right),
            Op::Complement | Op::Not | Op::Open | Op::Close | Op::Comma => {
                unreachable!("not a binary operator")
            }
        })
    }
    fn expect(&mut self, op: Op) -> AsmResult<()> {
        if self.peek() != Token::Op(op) {
            let text = match op {
                Op::Close => ")",
                _ => "operator",
            };
            return Err(format!("Expected '{text}'").into());
        }
        self.pos += 1;
        Ok(())
    }
    /// Evaluates a function call; `parens` is false for the prefix keyword form
    /// (`BANK label`), which has no closing parenthesis.
    fn function(&mut self, name: &str, parens: bool) -> AsmResult<u32> {
        let upper = name.to_ascii_uppercase();
        if upper == "REGIONSIZE" {
            let Token::String(region) = self.peek() else {
                return Err("REGIONSIZE requires a string".into());
            };
            let size = self.ctx.regions.get(region).and_then(|r| r.size);
            self.pos += 1;
            self.expect(Op::Close)?;
            return match size {
                Some(n) => Ok(n as u32),
                None if self.ctx.allow_undefined => Ok(0),
                None => Err("Region is undefined or incomplete".into()),
            };
        }
        if SYMBOL_KEYWORDS.contains(&upper.as_str()) {
            let Token::Name(symbol) = self.peek() else {
                return Err(format!("{name} requires a symbol").into());
            };
            let symbol = self.symbol(symbol).ok();
            self.pos += 1;
            if parens {
                self.expect(Op::Close)?;
            }
            if upper == "DEFINED" {
                return Ok(u32::from(symbol.is_some()));
            }
            let Some(s) = symbol else {
                return if self.ctx.allow_undefined {
                    Ok(0)
                } else {
                    Err("Undefined symbol".into())
                };
            };
            if !self.ctx.allow_undefined
                && ((upper == "BANK" && s.bank == BankRef::Constant)
                    || (upper == "SIZEOF" && s.data_type.is_none())
                    || upper == "VRAM"
                    || upper == "PAL")
            {
                return Err(format!("No {upper} attribute for this symbol").into());
            }
            return Ok(match upper.as_str() {
                "BANK" => u32::from(s.bank.number()),
                // Constants have no page; C# reports -1.
                "PAGE" => s.page.map_or(u32::MAX, |p| p as u32),
                "SIZEOF" => s.size as u32,
                _ => u32::MAX,
            });
        }
        let mut args = vec![self.expr(0)?];
        while self.peek() == Token::Op(Op::Comma) {
            self.pos += 1;
            args.push(self.expr(0)?);
        }
        self.expect(Op::Close)?;
        if let Some(body) = self.ctx.functions.get(name) {
            let calls = self.ctx.function_calls.get() + 1;
            if calls > FUNCTION_CALL_LIMIT {
                return Err(AsmError::fatal(
                    "Expression function evaluation limit exceeded",
                ));
            }
            self.ctx.function_calls.set(calls);
            let mut body = body.clone();
            for (i, n) in args.iter().enumerate() {
                body = body.replace(&format!("\\{}", i + 1), &format!("({n})"));
            }
            return evaluate_inner(&body, self.ctx, self.depth + 1);
        }
        if args.len() != 1 {
            return Err("Expected one argument".into());
        }
        match upper.as_str() {
            "LOW" => Ok(args[0] & 255),
            "HIGH" => Ok((args[0] >> 8) & 255),
            _ => Err(format!("Unknown function '{name}'").into()),
        }
    }
}

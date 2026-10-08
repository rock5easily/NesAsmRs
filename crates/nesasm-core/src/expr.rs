use crate::state::RESERVED_BANK;
use crate::{Region, Symbol};
use std::{cell::Cell, collections::BTreeMap};

/// Expression function calls allowed in one assembly, across both passes.
pub(crate) const FUNCTION_CALL_LIMIT: usize = 1_000_000;

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

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(u32),
    Name(String),
    String(String),
    Op(String),
    End,
}

pub(crate) fn evaluate(text: &str, ctx: &Context<'_>) -> Result<u32, String> {
    evaluate_inner(text, ctx, 0)
}
fn evaluate_inner(text: &str, ctx: &Context<'_>, depth: usize) -> Result<u32, String> {
    if depth > 32 {
        return Err("Expression function recursion limit exceeded".into());
    }
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch == '"' || ch == '\'' {
            let mut value = String::new();
            loop {
                match chars.next() {
                    Some(c) if c == ch => break,
                    Some(c) => value.push(c),
                    None => return Err("Unterminated literal".into()),
                }
            }
            if ch == '\'' {
                let bytes = value.as_bytes();
                if bytes.len() != 1 {
                    return Err("Character literal must be one byte".into());
                }
                tokens.push(Token::Number(bytes[0] as u32));
            } else {
                tokens.push(Token::String(value));
            }
        } else if ch.is_ascii_digit()
            || ch == '$'
            || (ch == '%'
                && tokens
                    .last()
                    .is_none_or(|t| matches!(t, Token::Op(op) if op != ")")))
        {
            let radix = if ch == '$' {
                16
            } else if ch == '%' {
                2
            } else {
                10
            };
            let mut value = String::new();
            if ch.is_ascii_digit() {
                value.push(ch);
            }
            while chars.peek().is_some_and(|c| c.is_digit(radix)) {
                value.push(chars.next().unwrap());
            }
            tokens.push(Token::Number(
                u32::from_str_radix(&value, radix).map_err(|_| "Invalid numeric literal")?,
            ));
        } else if ch.is_alphabetic() || ch == '_' || ch == '.' {
            let mut name = ch.to_string();
            while chars
                .peek()
                .is_some_and(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
            {
                name.push(chars.next().unwrap());
            }
            tokens.push(Token::Name(name));
        } else {
            let mut op = ch.to_string();
            if let Some(next) = chars.peek() {
                let pair = format!("{ch}{next}");
                if ["<<", ">>", "<=", ">=", "<>", "!=", "=="].contains(&pair.as_str()) {
                    op.push(chars.next().unwrap());
                }
            }
            if ![
                "+", "-", "*", "/", "%", "<<", ">>", "|", "^", "&", "~", "!", "=", "==", "<>",
                "!=", "<", "<=", ">", ">=", "(", ")", ",",
            ]
            .contains(&op.as_str())
            {
                return Err(format!("Invalid expression character '{ch}'"));
            }
            tokens.push(Token::Op(op));
        }
    }
    if tokens.len() > 256 {
        return Err("Expression complexity limit exceeded".into());
    }
    tokens.push(Token::End);
    let mut parser = Parser {
        tokens,
        pos: 0,
        ctx,
        depth,
    };
    let value = parser.expr(0)?;
    if parser.tokens[parser.pos] != Token::End {
        return Err("Unexpected expression suffix".into());
    }
    Ok(value)
}

struct Parser<'a, 'b> {
    tokens: Vec<Token>,
    pos: usize,
    ctx: &'a Context<'b>,
    depth: usize,
}
impl<'ctx, 'symbols> Parser<'ctx, 'symbols> {
    fn symbol(&self, name: &str) -> Result<&'symbols Symbol, String> {
        let local = name.starts_with('.');
        let key = if local {
            format!("{}{name}", self.ctx.global)
        } else {
            name.into()
        };
        self.ctx
            .symbols
            .get(&key)
            .filter(|s| local || !name.contains('.') || s.public)
            .ok_or_else(|| format!("Undefined symbol '{name}'"))
    }
    fn expr(&mut self, min: u8) -> Result<u32, String> {
        let token = self.tokens[self.pos].clone();
        self.pos += 1;
        let mut left = match token {
            Token::Number(n) => n,
            Token::Name(name) => {
                if self.tokens[self.pos] == Token::Op("(".into()) {
                    self.pos += 1;
                    self.function(&name)?
                } else {
                    match self.symbol(&name) {
                        Ok(s) => s.value,
                        Err(_) if self.ctx.allow_undefined => 0,
                        Err(e) => return Err(e),
                    }
                }
            }
            Token::Op(op) if op == "(" => {
                let n = self.expr(0)?;
                self.expect(")")?;
                n
            }
            Token::Op(op) if op == "*" => self.ctx.pc,
            Token::Op(op) if ["+", "-", "~", "!", "<", ">"].contains(&op.as_str()) => {
                let n = self.expr(10)?;
                match op.as_str() {
                    "-" => 0u32.wrapping_sub(n),
                    "~" => !n,
                    "!" => (n == 0) as u32,
                    "<" => n & 255,
                    ">" => (n >> 8) & 255,
                    _ => n,
                }
            }
            _ => return Err("Expected expression".into()),
        };
        while let Token::Op(op) = &self.tokens[self.pos] {
            let priority = match op.as_str() {
                "|" => 1,
                "^" => 2,
                "&" => 3,
                "=" | "==" | "<>" | "!=" => 4,
                "<" | "<=" | ">" | ">=" => 5,
                "<<" | ">>" => 6,
                "+" | "-" => 7,
                "*" | "/" | "%" => 8,
                _ => 0,
            };
            if priority == 0 || priority < min {
                break;
            }
            let op = op.clone();
            self.pos += 1;
            let right = self.expr(priority + 1)?;
            left = match op.as_str() {
                "+" => left.wrapping_add(right),
                "-" => left.wrapping_sub(right),
                "*" => left.wrapping_mul(right),
                "/" | "%" if right == 0 => {
                    if self.ctx.allow_undefined {
                        0
                    } else {
                        return Err("Division by zero".into());
                    }
                }
                "/" => (left as i32)
                    .checked_div(right as i32)
                    .ok_or("Division overflow")? as u32,
                "%" => (left as i32)
                    .checked_rem(right as i32)
                    .ok_or("Division overflow")? as u32,
                "<<" => left.wrapping_shl(right),
                ">>" => (left as i32).wrapping_shr(right) as u32,
                "|" => left | right,
                "^" => left ^ right,
                "&" => left & right,
                "=" | "==" => (left == right) as u32,
                "<>" | "!=" => (left != right) as u32,
                "<" => ((left as i32) < (right as i32)) as u32,
                "<=" => ((left as i32) <= (right as i32)) as u32,
                ">" => ((left as i32) > (right as i32)) as u32,
                ">=" => ((left as i32) >= (right as i32)) as u32,
                _ => unreachable!(),
            };
        }
        Ok(left)
    }
    fn expect(&mut self, op: &str) -> Result<(), String> {
        if self.tokens.get(self.pos) != Some(&Token::Op(op.into())) {
            return Err(format!("Expected '{op}'"));
        }
        self.pos += 1;
        Ok(())
    }
    fn function(&mut self, name: &str) -> Result<u32, String> {
        let upper = name.to_ascii_uppercase();
        if upper == "REGIONSIZE" {
            let Token::String(region) = &self.tokens[self.pos] else {
                return Err("REGIONSIZE requires a string".into());
            };
            let size = self.ctx.regions.get(region).and_then(|r| r.size);
            self.pos += 1;
            self.expect(")")?;
            return match size {
                Some(n) => Ok(n as u32),
                None if self.ctx.allow_undefined => Ok(0),
                None => Err("Region is undefined or incomplete".into()),
            };
        }
        if ["DEFINED", "BANK", "PAGE", "SIZEOF", "VRAM", "PAL"].contains(&upper.as_str()) {
            let Token::Name(symbol) = &self.tokens[self.pos] else {
                return Err(format!("{name} requires a symbol"));
            };
            let symbol = self.symbol(symbol).ok();
            self.pos += 1;
            self.expect(")")?;
            if upper == "DEFINED" {
                return Ok(symbol.is_some() as u32);
            }
            let Some(s) = symbol else {
                return if self.ctx.allow_undefined {
                    Ok(0)
                } else {
                    Err("Undefined symbol".into())
                };
            };
            if !self.ctx.allow_undefined
                && ((upper == "BANK" && s.bank == RESERVED_BANK)
                    || (upper == "SIZEOF" && s.data_type.is_none())
                    || upper == "VRAM"
                    || upper == "PAL")
            {
                return Err(format!("No {upper} attribute for this symbol"));
            }
            return Ok(match upper.as_str() {
                "BANK" => s.bank as u32,
                "PAGE" => s.page as u32,
                "SIZEOF" => s.size as u32,
                _ => u32::MAX,
            });
        }
        let mut args = vec![self.expr(0)?];
        while self.tokens[self.pos] == Token::Op(",".into()) {
            self.pos += 1;
            args.push(self.expr(0)?);
        }
        self.expect(")")?;
        if let Some(body) = self.ctx.functions.get(name) {
            let calls = self.ctx.function_calls.get() + 1;
            if calls > FUNCTION_CALL_LIMIT {
                return Err("Expression function evaluation limit exceeded".into());
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
            _ => Err(format!("Unknown function '{name}'")),
        }
    }
}

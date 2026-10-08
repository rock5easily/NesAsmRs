//! MACRO definitions and expansion.

use super::{Engine, PendingLine, Statement, extend_trace};
use crate::error::AsmError;
use crate::source::{self, Line};
use crate::state::{BANK_SIZE, MAX_NESTING};
use crate::{BankRef, DiagnosticCode, SourceLocation};
use std::{
    collections::{HashMap, VecDeque},
    fmt::Write,
    rc::Rc,
};

const MAX_ARGUMENTS: usize = 9;

#[derive(Default)]
pub(super) struct Macros {
    /// Bodies by upper-case name, shared by every expansion.
    definitions: HashMap<String, Rc<[Line]>>,
    /// Expansion counter for `\@`.
    counter: usize,
}

impl Macros {
    pub fn reset(&mut self) {
        self.definitions.clear();
        self.counter = 0;
    }
    pub fn contains(&self, name: &str) -> bool {
        self.definitions.contains_key(name)
    }
    pub fn get(&self, name: &str) -> Option<Rc<[Line]>> {
        self.definitions.get(name).cloned()
    }
}

/// Value of `\?n`: the kind of the n-th argument (legacy numbering).
#[derive(Clone, Copy)]
#[repr(u8)]
enum ArgumentKind {
    None = 0,
    Register = 1,
    Immediate = 2,
    Constant = 3,
    Indirect = 4,
    String = 5,
    Label = 6,
}

impl Engine<'_> {
    /// Reads a macro body up to ENDM. The definition must close before its
    /// source file returns.
    /// Returns true when the definition is unterminated, which is fatal.
    pub(super) fn define_macro(
        &mut self,
        line: &Line,
        statement: &Statement,
        queue: &mut VecDeque<PendingLine>,
    ) -> bool {
        let name = statement
            .label
            .clone()
            .unwrap_or_else(|| statement.operand.trim().into());
        // As in the C# version an invalid name is a label error: the body is not
        // read, so its lines are assembled (and a later ENDM is unexpected).
        if !name.is_empty() && !valid_macro_name(&name) {
            self.line_error(line, DiagnosticCode::Macro, "Invalid macro name");
            return false;
        }
        let listed = self.pass.is_emitting() && self.listing.enabled;
        if listed {
            self.list_line(line, self.position, None, &[], false, None);
        }
        let mut body = Vec::new();
        let mut found = false;
        while let Some(PendingLine::Source(next)) = queue.pop_front() {
            if !next.expanded {
                self.listing.line_number = next.line;
            }
            if listed {
                self.list_line(&next, self.position, None, &[], false, None);
            }
            if self.parse(source::strip_comment(&next.text)).op == "ENDM" {
                found = true;
                break;
            }
            body.push(next);
        }
        if !found {
            // Reported, like C#, at the end of the file the definition started in.
            let end = self
                .cache
                .sources
                .get(&*line.file)
                .and_then(|(_, text)| text.last().map(|(n, _)| n + 1))
                .unwrap_or(line.line);
            let at = SourceLocation {
                line: end,
                ..line.location()
            };
            self.error_at(
                &at,
                &line.trace,
                DiagnosticCode::Macro,
                &format!(
                    "Incomplete MACRO definition (started at line {})",
                    line.line
                ),
            );
            return true;
        }
        let key = name.to_ascii_uppercase();
        if name.is_empty() || self.macros.contains(&key) {
            self.line_error(line, DiagnosticCode::Macro, "Invalid or duplicate macro");
            return false;
        }
        self.macros.definitions.insert(key, body.into());
        false
    }

    /// Expands a macro call into the lines to assemble next.
    pub(super) fn expand_macro(
        &mut self,
        line: &Line,
        statement: &Statement,
        body: &[Line],
    ) -> Result<Vec<Line>, (DiagnosticCode, AsmError)> {
        if self.listing.shows(self.pass, line) {
            let mut position = self.position;
            // Without MLIST the call line shows the CPU address in the offset field.
            if !self.listing.macros {
                position.offset += position.page * BANK_SIZE;
            }
            self.list_line(line, position, None, &[], !self.listing.macros, None);
        }
        if line.trace.len() > MAX_NESTING {
            return Err((
                DiagnosticCode::Limit,
                AsmError::fatal("Macro nesting limit exceeded"),
            ));
        }
        if let Some(name) = statement.label.as_deref() {
            self.define(name, self.pc(), line, false)
                .map_err(|e| (DiagnosticCode::Symbol, e))?;
        }
        let args = source::macro_arguments(&statement.operand)
            .map_err(|e| (DiagnosticCode::Macro, e.into()))?;
        if args.len() > MAX_ARGUMENTS {
            return Err((DiagnosticCode::Macro, "Maximum nine macro arguments".into()));
        }
        self.macros.counter += 1;
        let kinds: Vec<ArgumentKind> = (0..MAX_ARGUMENTS)
            .map(|i| self.argument_kind(args.get(i).map_or("", String::as_str)))
            .collect();
        // \# is the index of the last non-empty argument.
        let count = args
            .iter()
            .rposition(|a| !a.is_empty())
            .map_or(0, |i| i + 1);
        // Expanded lines share one trace instead of copying it per line.
        let trace = extend_trace(line);
        Ok(body
            .iter()
            .map(|template| Line {
                text: substitute(&template.text, &args, &kinds, self.macros.counter, count),
                file: Rc::clone(&template.file),
                line: template.line,
                trace: Rc::clone(&trace),
                expanded: true,
            })
            .collect())
    }

    fn argument_kind(&self, arg: &str) -> ArgumentKind {
        if arg.is_empty() {
            ArgumentKind::None
        } else if arg.starts_with('#') {
            ArgumentKind::Immediate
        } else if arg.starts_with('"') {
            ArgumentKind::String
        } else if arg.starts_with('[') {
            ArgumentKind::Indirect
        } else if ["A", "X", "Y"].contains(&arg.to_ascii_uppercase().as_str()) {
            ArgumentKind::Register
        } else if arg
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
            && !arg.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            let constant = if arg.starts_with('.') {
                self.result.symbols.get(&format!("{}{arg}", self.scope))
            } else {
                self.result.symbols.get(arg)
            }
            .is_some_and(|s| s.bank == BankRef::Constant);
            if constant {
                ArgumentKind::Constant
            } else {
                ArgumentKind::Label
            }
        } else {
            ArgumentKind::Constant
        }
    }
}

/// A macro name is a global label name.
fn valid_macro_name(name: &str) -> bool {
    name.len() <= 64
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
}

/// Replaces `\1`-`\9`, `\?1`-`\?9`, `\#` and `\@` in one pass, so text that an
/// argument brings in is never substituted again.
fn substitute(
    text: &str,
    args: &[String],
    kinds: &[ArgumentKind],
    counter: usize,
    count: usize,
) -> String {
    let digit = |c: Option<char>| {
        c.and_then(|c| c.to_digit(10))
            .filter(|d| (1..=9).contains(d))
            .map(|d| d as usize - 1)
    };
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('@') => {
                chars.next();
                let _ = write!(out, "{counter:05}");
            }
            Some('#') => {
                chars.next();
                out.push_str(&count.to_string());
            }
            Some('?') => {
                let mut ahead = chars.clone();
                ahead.next();
                if let Some(index) = digit(ahead.next()) {
                    chars.next();
                    chars.next();
                    out.push_str(&(kinds[index] as u8).to_string());
                } else {
                    out.push(c);
                }
            }
            next => match digit(next) {
                Some(index) => {
                    chars.next();
                    out.push_str(args.get(index).map_or("", String::as_str));
                }
                None => out.push(c),
            },
        }
    }
    out
}

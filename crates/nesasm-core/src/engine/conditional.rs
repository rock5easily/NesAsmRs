//! IF / IFDEF / IFNDEF / ELSE / ENDIF.

use super::{Directive, Engine, Statement};
use crate::DiagnosticCode;
use crate::SourceLocation;
use crate::error::AsmResult;
use crate::source::Line;
use crate::state::{MAX_CONDITIONAL_NESTING, Pass};
use std::collections::BTreeSet;

struct Conditional {
    /// Whether the enclosing block is active.
    parent: bool,
    condition: bool,
    otherwise: bool,
    /// Where the IF was opened, for "Missing ENDIF".
    location: SourceLocation,
}

#[derive(Default)]
pub(super) struct Conditions {
    stack: Vec<Conditional>,
    /// IF results from the layout pass, compared in order during the emit pass.
    results: Vec<bool>,
    index: usize,
    /// Symbols tested by IFDEF/IFNDEF before they were defined.
    undefined: BTreeSet<String>,
}

impl Conditions {
    pub fn reset(&mut self, pass: Pass) {
        self.stack.clear();
        self.undefined.clear();
        if pass.is_layout() {
            self.results.clear();
        }
        self.index = 0;
    }
    /// Whether lines are currently assembled.
    pub fn active(&self) -> bool {
        self.stack
            .last()
            .is_none_or(|c| c.parent && (c.condition != c.otherwise))
    }
    fn parent_active(&self) -> bool {
        self.stack.last().is_none_or(|c| c.parent)
    }
    pub fn open_location(&self) -> Option<SourceLocation> {
        self.stack.last().map(|c| c.location.clone())
    }
    pub fn declared_undefined(&self, key: &str) -> bool {
        self.undefined.contains(key)
    }
}

impl Engine<'_> {
    /// Handles a conditional directive line, including its listing.
    pub(super) fn conditional_line(
        &mut self,
        line: &Line,
        statement: &Statement,
        directive: Directive,
    ) {
        let operand = &statement.operand;
        let display = match directive {
            Directive::If => self.value(operand).ok(),
            Directive::Ifdef | Directive::Ifndef => Some(
                u32::from(self.result.symbols.contains_key(operand.as_str()))
                    ^ u32::from(directive == Directive::Ifndef),
            ),
            _ => None,
        };
        let parent_active = self.conditions.parent_active();
        // Like the C# DoIf/DoIfdef, an active IF line defines its label.
        if let Some(name) = statement.label.as_deref()
            && matches!(
                directive,
                Directive::If | Directive::Ifdef | Directive::Ifndef
            )
            && self.conditions.active()
            && let Err(e) = self.define(name, self.pc(), line, false)
        {
            self.line_error(line, DiagnosticCode::Symbol, &e.message);
        }
        if let Err(e) = self.conditional(directive, operand, line) {
            self.line_error(line, DiagnosticCode::Conditional, &e.message);
            return;
        }
        if self.listing.shows(self.pass, line) && parent_active {
            self.list_line(line, self.position, Some(directive), &[], false, display);
        }
    }

    fn conditional(&mut self, directive: Directive, operand: &str, line: &Line) -> AsmResult<()> {
        match directive {
            Directive::If | Directive::Ifdef | Directive::Ifndef => {
                if self.conditions.stack.len() > MAX_CONDITIONAL_NESTING {
                    return Err("Conditional nesting limit exceeded".into());
                }
                let parent = self.conditions.active();
                let condition = if !parent {
                    false
                } else if directive == Directive::If {
                    self.if_condition(operand)?
                } else {
                    let key = if operand.starts_with('.') {
                        format!("{}{operand}", self.scope)
                    } else {
                        operand.into()
                    };
                    let defined = self.result.symbols.contains_key(&key);
                    if self.pass.is_layout() && !defined {
                        self.conditions.undefined.insert(key);
                    }
                    defined != (directive == Directive::Ifndef)
                };
                self.conditions.stack.push(Conditional {
                    parent,
                    condition,
                    otherwise: false,
                    location: line.location(),
                });
            }
            Directive::Else => {
                if !operand.is_empty() {
                    return Err("Unexpected ELSE operand".into());
                }
                let c = self.conditions.stack.last_mut().ok_or("Unexpected ELSE")?;
                if c.otherwise {
                    return Err("Duplicate ELSE".into());
                }
                c.otherwise = true;
            }
            Directive::Endif => {
                if !operand.is_empty() {
                    return Err("Unexpected ENDIF operand".into());
                }
                self.conditions.stack.pop().ok_or("Unexpected ENDIF")?;
            }
            _ => unreachable!("not a conditional directive"),
        }
        Ok(())
    }

    /// Evaluates an IF condition; it must not change between the passes.
    fn if_condition(&mut self, operand: &str) -> AsmResult<bool> {
        let condition = self.value(operand)? != 0;
        if self.pass.is_layout() {
            self.conditions.results.push(condition);
        } else {
            let layout = self.conditions.results.get(self.conditions.index).copied();
            self.conditions.index += 1;
            if layout != Some(condition) {
                return Err("IF condition changed between passes (forward reference)".into());
            }
        }
        Ok(condition)
    }
}

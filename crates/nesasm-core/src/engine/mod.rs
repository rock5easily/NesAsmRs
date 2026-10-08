//! Two-pass assembler. `mod.rs` holds the engine state, the per-line driver and
//! symbol definition; directives, instructions, macros, procedures, conditional
//! assembly, the listing and the ROM image live in the submodules.

mod conditional;
mod directive;
mod instruction;
mod listing;
mod macros;
mod procedure;
mod rom;

use crate::error::AsmResult;
use crate::source::{self, Line, SourceText};
use crate::state::{
    BANK_SIZE, BSS_START, MAX_BANKS, MAX_NESTING, PROCEDURE_BANK, Pass, START_PAGE, STEP_LIMIT,
    Section,
};
use crate::{
    AssembleRequest, AssembleResult, BankRef, DataType, Diagnostic, DiagnosticCode, RamUsage,
    Severity, SourceLocation, Symbol, expr,
};
use conditional::Conditions;
use directive::Directive;
use listing::Listing;
use macros::Macros;
use procedure::Procedures;
use rom::RomImage;
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    fmt::Write,
    iter,
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

/// Physical assembly position. The label scope is kept separately in `scope`.
#[derive(Clone, Copy, Default)]
struct Position {
    bank: usize,
    page: usize,
    offset: usize,
}

impl Position {
    /// CPU address.
    const fn pc(self) -> usize {
        self.page * BANK_SIZE + self.offset
    }
    /// Offset in the ROM image.
    const fn linear(self) -> usize {
        self.bank * BANK_SIZE + self.offset
    }
}

/// A position and the label scope that was current there.
#[derive(Clone)]
struct Cursor {
    position: Position,
    scope: Rc<str>,
}

enum PendingLine {
    Source(Line),
    ReturnFromInclude { depth: usize, name: String },
}

/// One source line split into label, operation (upper case, without a leading
/// dot) and operand.
struct Statement {
    label: Option<String>,
    op: String,
    operand: String,
}

/// Decoded sources and converted images, shared by both passes so each file
/// is read once and both passes see the same contents.
#[derive(Default)]
struct SourceCache {
    sources: HashMap<PathBuf, (Rc<Path>, SourceText)>,
    tiles: HashMap<(PathBuf, Vec<usize>), Rc<[u8]>>,
}

struct Engine<'a> {
    request: &'a AssembleRequest,
    cancel: &'a AtomicBool,
    result: AssembleResult,
    pass: Pass,
    position: Position,
    /// Global label that local (`.name`) labels belong to.
    scope: Rc<str>,
    section: Section,
    saved: BTreeMap<(Section, usize), Cursor>,
    section_bank: [usize; 4],
    cat: BTreeSet<usize>,
    rs: u32,
    functions: BTreeMap<String, String>,
    defined: BTreeSet<String>,
    auto_zp: bool,
    warn: bool,
    header: [u8; 16],
    last_data: Option<(String, DataType)>,
    input_names: BTreeMap<PathBuf, (usize, String)>,
    bank_names: BTreeMap<usize, String>,
    /// Bytes each executed line advanced in the layout pass, replayed on emit errors.
    line_sizes: Vec<Option<usize>>,
    line_index: usize,
    function_calls: Cell<usize>,
    max_bss: usize,
    max_zp: usize,
    rom: RomImage,
    listing: Listing,
    procs: Procedures,
    macros: Macros,
    conditions: Conditions,
    cache: SourceCache,
}

pub fn assemble(request: &AssembleRequest) -> AssembleResult {
    assemble_with_cancel(request, &AtomicBool::new(false))
}

/// Assembles like [`assemble`], stopping with an `E_CANCELLED` error once
/// `cancel` is set. The flag is checked before each source line.
pub fn assemble_with_cancel(request: &AssembleRequest, cancel: &AtomicBool) -> AssembleResult {
    let mut engine = Engine::new(request, cancel);
    let location = SourceLocation::file(&request.input);
    for (name, value) in [
        ("MAGICKIT", 1),
        ("DEVELO", 0),
        ("CDROM", 0),
        ("_bss_end", 0),
        ("_bank_base", 0),
        ("_nb_bank", 1),
        ("_call_bank", 0),
    ] {
        engine.reserve(name, value);
    }
    let path = match source::find_file(request, &request.input) {
        Ok(p) => p,
        Err(e) => {
            engine.error_at(&location, &[], DiagnosticCode::Io, &e);
            return engine.result;
        }
    };
    if let Err(e) = engine.load(&path, &Rc::from([])) {
        engine.error_at(&location, &[], DiagnosticCode::Io, &e.message);
        return engine.result;
    }
    engine
        .input_names
        .insert(path.clone(), (1, request.input.display().to_string()));
    for pass in [Pass::Layout, Pass::Emit] {
        engine.pass = pass;
        engine.reset();
        let lines = engine
            .load(&path, &Rc::from([]))
            .expect("source cached by the first load");
        // After a fatal error the open blocks are an artifact of stopping early.
        let stopped = engine.run(lines);
        if !stopped && let Some(at) = engine.conditions.open_location() {
            engine.error_at(&at, &[], DiagnosticCode::Conditional, "Missing ENDIF");
        }
        if !stopped && let Some(at) = engine.procs.open_location() {
            engine.error_at(
                &at,
                &[],
                DiagnosticCode::Procedure,
                "Missing ENDP/ENDPROCGROUP",
            );
        }
        if engine.result.error_count() > 0 {
            break;
        }
        if pass == Pass::Layout {
            engine.relocate();
            engine.reserve("_bss_end", engine.max_bss as u32);
            engine.reserve("_nb_bank", (engine.rom.max_bank + 1) as u32);
        }
    }
    engine.finish()
}

impl<'a> Engine<'a> {
    fn new(request: &'a AssembleRequest, cancel: &'a AtomicBool) -> Self {
        Self {
            request,
            cancel,
            result: AssembleResult::default(),
            pass: Pass::Layout,
            position: Position::default(),
            scope: Rc::from(""),
            section: Section::Code,
            saved: BTreeMap::new(),
            section_bank: [0; 4],
            cat: BTreeSet::new(),
            rs: 0,
            functions: BTreeMap::new(),
            defined: BTreeSet::new(),
            auto_zp: request.options.auto_zp,
            warn: false,
            header: [0; 16],
            last_data: None,
            input_names: BTreeMap::new(),
            bank_names: BTreeMap::new(),
            line_sizes: Vec::new(),
            line_index: 0,
            function_calls: Cell::new(0),
            max_bss: BSS_START + 1,
            max_zp: 1,
            rom: RomImage::default(),
            listing: Listing::default(),
            procs: Procedures::default(),
            macros: Macros::default(),
            conditions: Conditions::default(),
            cache: SourceCache::default(),
        }
    }

    /// Builds the result after both passes.
    fn finish(mut self) -> AssembleResult {
        self.result.success = self.result.error_count() == 0;
        if !self.result.success {
            return self.result;
        }
        let len = (self.rom.max_bank + 1) * BANK_SIZE;
        self.result.banks = (0..=self.rom.max_bank)
            .map(|bank| {
                self.rom
                    .bank_usage(bank, self.bank_names.get(&bank).cloned())
            })
            .collect();
        let (binary, map) = std::mem::take(&mut self.rom).into_parts(len);
        self.result.binary = binary;
        self.result.map = map;
        self.header[..4].copy_from_slice(b"NES\x1a");
        let options = &self.request.options;
        if !options.raw && !options.srec {
            self.result.header = self.header.to_vec();
        }
        self.result.ram = RamUsage {
            zero_page_end: self.max_zp,
            bss_end: self.max_bss,
        };
        if self.listing.requested && options.list_level > crate::ListLevel::Off {
            self.result.listing = Some(std::mem::take(&mut self.listing.text));
        }
        if options.srec {
            self.result.srec = Some(crate::output::srec(&self.result.binary, &self.result.map));
        }
        self.result
    }

    fn reset(&mut self) {
        self.position = Position {
            page: START_PAGE,
            ..Position::default()
        };
        self.scope = Rc::from("");
        self.section = Section::Code;
        self.section_bank = [0; 4];
        self.saved.clear();
        let cursor = |position| Cursor {
            position,
            scope: Rc::from(""),
        };
        self.saved
            .insert((Section::ZeroPage, 0), cursor(Position::default()));
        self.saved.insert(
            (Section::Bss, 0),
            cursor(Position {
                offset: BSS_START,
                ..Position::default()
            }),
        );
        self.saved.insert((Section::Code, 0), cursor(self.position));
        self.saved.insert((Section::Data, 0), cursor(self.position));
        self.conditions.reset(self.pass);
        self.procs.reset();
        self.macros.reset();
        self.functions.clear();
        self.defined.clear();
        if self.pass.is_layout() {
            self.line_sizes.clear();
        }
        self.line_index = 0;
        self.cat.clear();
        self.rs = 0;
        self.listing.reset(self.request);
        self.auto_zp = self.request.options.auto_zp;
        self.warn = false;
        self.last_data = None;
    }

    fn reserve(&mut self, name: &str, value: u32) {
        self.result.symbols.insert(
            name.into(),
            Symbol {
                name: name.into(),
                value,
                bank: BankRef::Constant,
                page: None,
                location: SourceLocation::default(),
                public: true,
                size: 0,
                data_type: None,
            },
        );
    }

    fn error_at(
        &mut self,
        location: &SourceLocation,
        trace: &[SourceLocation],
        code: DiagnosticCode,
        message: &str,
    ) {
        self.result.diagnostics.push(Diagnostic {
            severity: Severity::Error,
            code,
            message: message.into(),
            location: location.clone(),
            expansion_trace: trace.into(),
        });
    }

    fn line_error(&mut self, line: &Line, code: DiagnosticCode, message: &str) {
        self.error_at(&line.location(), &line.trace, code, message);
    }

    /// Lines of a source file as included from `trace`, reading it on first use.
    fn load(&mut self, path: &Path, trace: &Rc<[SourceLocation]>) -> AsmResult<Vec<Line>> {
        if !self.result.dependencies.iter().any(|p| p == path) {
            self.result.dependencies.push(path.into());
        }
        if !self.cache.sources.contains_key(path) {
            let text = source::read_source(self.request, path)?;
            self.cache
                .sources
                .insert(path.into(), (Rc::from(path), text));
        }
        let (file, text) = &self.cache.sources[path];
        Ok(source::lines(text, file, trace))
    }

    /// Records a dependency that is read directly (INCBIN, INCCHR).
    fn depend(&mut self, path: &Path) {
        if !self.result.dependencies.iter().any(|p| p == path) {
            self.result.dependencies.push(path.into());
        }
    }

    fn context(&self, allow_undefined: bool) -> expr::Context<'_> {
        expr::Context {
            symbols: &self.result.symbols,
            regions: &self.result.regions,
            functions: &self.functions,
            global: &self.scope,
            pc: self.pc(),
            allow_undefined,
            function_calls: &self.function_calls,
        }
    }

    /// Evaluates an expression; undefined symbols are 0 in the layout pass.
    fn value(&self, text: &str) -> AsmResult<u32> {
        expr::evaluate(text, &self.context(self.pass.is_layout()))
    }

    /// Evaluates an expression whose symbols must already be defined.
    fn strict_value(&self, text: &str) -> AsmResult<u32> {
        expr::evaluate(text, &self.context(false))
    }

    fn pc(&self) -> u32 {
        self.position.pc() as u32
    }

    /// The symbol bank for the current position.
    fn current_bank(&self) -> BankRef {
        if self.section.is_ram() {
            BankRef::Constant
        } else {
            bank_ref(self.position.bank)
        }
    }

    /// Defines `name` at `value` (layout pass) or checks it is unchanged (emit
    /// pass), and makes a global label the new local-label scope.
    fn define(&mut self, name: &str, value: u32, line: &Line, public: bool) -> AsmResult<String> {
        let local = name.starts_with('.');
        if local && self.scope.is_empty() {
            return Err("Local label has no global scope".into());
        }
        if !local && self.macros.contains(&name.to_ascii_uppercase()) {
            return Err("Symbol already used by a macro".into());
        }
        if name.is_empty()
            || name.len() > 64
            || !name
                .trim_start_matches('.')
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_')
            || name.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            return Err("Invalid label".into());
        }
        let key = if local {
            format!("{}{name}", self.scope)
        } else {
            name.into()
        };
        if self.pass.is_layout() && self.conditions.declared_undefined(&key) {
            return Err("Cannot define a symbol declared undefined by IFDEF/IFNDEF".into());
        }
        if !self.defined.insert(key.clone()) {
            if self
                .result
                .symbols
                .get(&key)
                .is_some_and(|s| s.value == value)
            {
                return Ok(key);
            }
            return Err(format!("Symbol '{key}' is multiply defined").into());
        }
        let bank = self.current_bank();
        if self.pass.is_layout() {
            if self
                .result
                .symbols
                .get(&key)
                .is_some_and(|s| s.location.line == 0)
            {
                return Err("Reserved symbol cannot be redefined".into());
            }
            self.result.symbols.insert(
                key.clone(),
                Symbol {
                    name: key.clone(),
                    value,
                    bank,
                    page: Some(self.position.page),
                    location: line.location(),
                    public: public || !local,
                    size: 0,
                    data_type: None,
                },
            );
            self.procs.register_symbol(&key);
        } else {
            let s = self
                .result
                .symbols
                .get_mut(&key)
                .ok_or_else(|| format!("Phase error for symbol '{key}'"))?;
            if s.value != value {
                return Err(format!("Phase error for symbol '{key}'").into());
            }
            // Constants and RAM labels carry no bank; address labels must stay in theirs.
            if s.bank != BankRef::Constant && s.bank != bank {
                return Err(format!("Bank mismatch for symbol '{key}'").into());
            }
            s.public |= public;
        }
        if !local {
            self.scope = Rc::from(key.as_str());
        }
        Ok(key)
    }

    /// Defines a constant (EQU, RS): no bank, no page, and no new label scope.
    fn define_constant(&mut self, name: &str, value: u32, line: &Line) -> AsmResult<String> {
        let scope = Rc::clone(&self.scope);
        let key = self.define(name, value, line, false)?;
        let symbol = self
            .result
            .symbols
            .get_mut(&key)
            .ok_or_else(|| format!("Phase error for symbol '{key}'"))?;
        symbol.bank = BankRef::Constant;
        symbol.page = None;
        self.scope = scope;
        Ok(key)
    }

    /// Runs one pass. Line errors are reported and the pass continues, as in the
    /// C# version; returns true when a fatal error stopped the pass early.
    fn run(&mut self, lines: Vec<Line>) -> bool {
        let mut queue: VecDeque<PendingLine> = lines.into_iter().map(PendingLine::Source).collect();
        let mut steps = 0usize;
        while let Some(line) = self.next_source_line(&mut queue) {
            steps += 1;
            if self.cancel.load(Ordering::Relaxed) {
                self.line_error(&line, DiagnosticCode::Cancelled, "Assembly cancelled");
                return true;
            }
            if steps > STEP_LIMIT {
                self.line_error(
                    &line,
                    DiagnosticCode::Limit,
                    "Expanded source limit exceeded",
                );
                return true;
            }
            if !line.expanded {
                self.listing.line_number = line.line;
            }
            let statement = self.parse(source::strip_comment(&line.text));
            let directive = Directive::parse(&statement.op);
            if directive.is_some_and(Directive::is_conditional) {
                self.conditional_line(&line, &statement, directive.unwrap());
                continue;
            }
            if !self.conditions.active() {
                continue;
            }
            if directive == Some(Directive::Macro) {
                if self.define_macro(&line, &statement, &mut queue) {
                    return true;
                }
                continue;
            }
            if let Some(body) = self.macros.get(&statement.op) {
                match self.expand_macro(&line, &statement, &body) {
                    Ok(expanded) => {
                        for next in expanded.into_iter().rev() {
                            queue.push_front(PendingLine::Source(next));
                        }
                    }
                    Err((code, e)) => {
                        self.line_error(&line, code, &e.message);
                        if e.fatal {
                            return true;
                        }
                    }
                }
                continue;
            }
            if directive == Some(Directive::Include) {
                match self.include(&line, &statement) {
                    Ok(lines) => {
                        let header = self
                            .input_names
                            .get(&*line.file)
                            .cloned()
                            .unwrap_or((1, self.request.input.display().to_string()));
                        queue.push_front(PendingLine::ReturnFromInclude {
                            depth: header.0,
                            name: header.1,
                        });
                        for next in lines.into_iter().rev() {
                            queue.push_front(PendingLine::Source(next));
                        }
                    }
                    Err(e) => {
                        self.line_error(&line, DiagnosticCode::Include, &e.message);
                        return true;
                    }
                }
                continue;
            }
            if self.execute_line(&line, &statement, directive) {
                return true;
            }
        }
        false
    }

    /// Executes a directive or instruction line; returns true on a fatal error.
    fn execute_line(
        &mut self,
        line: &Line,
        statement: &Statement,
        directive: Option<Directive>,
    ) -> bool {
        let start = self.position;
        let rs_before = self.rs;
        let data = self.execute(statement, directive, line);
        // Keep later addresses stable when an emit-pass line fails: advance by the
        // size the layout pass recorded for it instead of emitting nothing.
        let advanced = (self.position.bank == start.bank)
            .then(|| self.position.offset.checked_sub(start.offset))
            .flatten();
        if self.pass.is_layout() {
            self.line_sizes.push(advanced);
        } else {
            let layout = self.line_sizes.get(self.line_index).copied().flatten();
            self.line_index += 1;
            if data.is_err()
                && let Some(size) = layout
            {
                self.position = start;
                self.position.offset += size;
            }
        }
        match data {
            Ok(bytes) => {
                if self.listing.shows(self.pass, line)
                    && !matches!(
                        directive,
                        Some(Directive::List | Directive::Mlist | Directive::Nomlist)
                    )
                {
                    // Like the C# listing: constants and RS show their value,
                    // FUNC definitions show no address.
                    let value = match directive {
                        Some(Directive::Equ) => self.value(&statement.operand).ok(),
                        Some(Directive::Rs) => Some(rs_before),
                        _ => None,
                    };
                    let has_label = statement.label.is_some() && directive != Some(Directive::Func);
                    self.list_line(line, start, directive, &bytes, has_label, value);
                }
                false
            }
            Err(e) => {
                self.line_error(line, DiagnosticCode::Assembly, &e.message);
                e.fatal
            }
        }
    }

    fn next_source_line(&mut self, queue: &mut VecDeque<PendingLine>) -> Option<Line> {
        loop {
            match queue.pop_front()? {
                PendingLine::Source(line) => return Some(line),
                PendingLine::ReturnFromInclude { depth, name } => {
                    if self.listing.writes_files(self.pass, self.request) {
                        let _ = writeln!(self.listing.text, "#[{depth}]   {name}");
                    }
                }
            }
        }
    }

    /// Includes a source file; errors are fatal for the pass.
    fn include(&mut self, line: &Line, statement: &Statement) -> AsmResult<Vec<Line>> {
        if let Some(name) = statement.label.as_deref() {
            self.define(name, self.pc(), line, false)?;
        }
        let mut name = source::quoted(&statement.operand)?;
        if !Path::new(&name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("asm"))
        {
            name.push_str(".asm");
        }
        let path = source::find_file(self.request, Path::new(&name))?;
        let trace = extend_trace(line);
        if trace.len() > MAX_NESTING {
            return Err("Include nesting limit exceeded".into());
        }
        let (depth, _) = self
            .input_names
            .get(&*line.file)
            .cloned()
            .unwrap_or((1, String::new()));
        self.input_names
            .insert(path.clone(), (depth + 1, name.clone()));
        if self.listing.writes_files(self.pass, self.request) {
            let _ = writeln!(self.listing.text, "#[{}]   {name}", depth + 1);
            if self.listing.enabled {
                self.list_line(line, self.position, None, &[], false, None);
            }
        }
        self.load(&path, &trace)
    }

    fn parse(&self, text: &str) -> Statement {
        let statement = |label: Option<&str>, op: &str, operand: &str| Statement {
            label: label.map(Into::into),
            op: op.into(),
            operand: operand.into(),
        };
        let indented = text.starts_with(char::is_whitespace);
        if text.starts_with('*') {
            return statement(None, "", "");
        }
        let mut text = text.trim();
        if text.is_empty() {
            return statement(None, "", "");
        }
        if let Some((name, value)) = text.split_once('=') {
            let name = name.trim();
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
            {
                return statement(Some(name), "=", value.trim());
            }
        }
        let mut label = None;
        let (first, rest) = word(text);
        if let Some((name, tail)) = first.split_once(':') {
            label = Some(name);
            text = if tail.is_empty() {
                rest
            } else {
                &text[name.len() + 1..]
            };
        } else if let Some(value) = rest.trim_start().strip_prefix('=') {
            return statement(Some(first), "=", value.trim());
        } else if !indented {
            // As in the C# version, a word in column 1 is always a label,
            // even when it spells an instruction, directive or macro.
            label = Some(first);
            text = rest;
        }
        let (op, rest) = word(text);
        Statement {
            label: label.map(Into::into),
            op: op.trim_start_matches('.').to_ascii_uppercase(),
            operand: rest.trim().into(),
        }
    }
}

/// The trace for lines included or expanded from `line`.
fn extend_trace(line: &Line) -> Rc<[SourceLocation]> {
    line.trace
        .iter()
        .cloned()
        .chain(iter::once(line.location()))
        .collect()
}

/// The symbol bank for an assembler bank number.
fn bank_ref(bank: usize) -> BankRef {
    if bank == PROCEDURE_BANK {
        return BankRef::Procedure;
    }
    u8::try_from(bank)
        .ok()
        .filter(|b| usize::from(*b) < MAX_BANKS)
        .map_or(BankRef::Procedure, BankRef::Rom)
}

fn word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    text.split_once(char::is_whitespace)
        .map_or((text, ""), |(a, b)| (a, b.trim_start()))
}

use crate::{
    AssembleRequest, AssembleResult, BankUsage, DataType, Diagnostic, Region, Severity,
    SourceLocation, Symbol,
};
use crate::{
    expr, image,
    opcode::{self, Mode},
    source::{self, Line},
    state::{PROCEDURE_BANK, Pass, RESERVED_BANK, Section},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

const BANK_SIZE: usize = 8192;
const LIMIT: usize = 128 * BANK_SIZE;

#[derive(Clone, Default)]
struct Position {
    bank: usize,
    page: usize,
    offset: usize,
    global: String,
}
#[derive(Clone)]
struct Procedure {
    name: String,
    base: usize,
    bank: usize,
    org: usize,
    size: usize,
    group: Option<String>,
}
struct ProcFrame {
    name: String,
    saved: Position,
    group: bool,
}
struct Conditional {
    parent: bool,
    condition: bool,
    otherwise: bool,
}

enum PendingLine {
    Source(Line),
    ReturnFromInclude { depth: usize, name: String },
}

struct Engine<'a> {
    request: &'a AssembleRequest,
    result: AssembleResult,
    pass: Pass,
    position: Position,
    section: Section,
    saved: BTreeMap<(Section, usize), Position>,
    section_bank: [usize; 4],
    cat: BTreeSet<usize>,
    max_bank: usize,
    rs: u32,
    macros: BTreeMap<String, Vec<Line>>,
    functions: BTreeMap<String, String>,
    procedures: Vec<Procedure>,
    frames: Vec<ProcFrame>,
    symbol_proc: BTreeMap<String, String>,
    defined: BTreeSet<String>,
    conditions: Vec<Conditional>,
    auto_zp: bool,
    list: bool,
    any_list: bool,
    mlist: bool,
    warn: bool,
    listing: String,
    line_number: usize,
    macro_counter: usize,
    header: [u8; 16],
    last_data: Option<(String, DataType)>,
    call_bank: Option<usize>,
    call_bytes: Vec<u8>,
    calls: BTreeMap<String, u32>,
    occupied: Vec<bool>,
    input_names: BTreeMap<std::path::PathBuf, (usize, String)>,
    bank_names: BTreeMap<usize, String>,
    if_undefined: BTreeSet<String>,
    max_bss: usize,
}

pub fn assemble(request: &AssembleRequest) -> AssembleResult {
    let mut engine = Engine {
        request,
        result: AssembleResult::default(),
        pass: Pass::Layout,
        position: Position::default(),
        section: Section::Code,
        saved: BTreeMap::new(),
        section_bank: [0; 4],
        cat: BTreeSet::new(),
        max_bank: 0,
        rs: 0,
        macros: BTreeMap::new(),
        functions: BTreeMap::new(),
        procedures: Vec::new(),
        frames: Vec::new(),
        symbol_proc: BTreeMap::new(),
        defined: BTreeSet::new(),
        conditions: Vec::new(),
        auto_zp: request.options.auto_zp,
        list: false,
        any_list: false,
        mlist: request.options.macro_listing,
        warn: false,
        listing: String::new(),
        line_number: 0,
        macro_counter: 0,
        header: [0; 16],
        last_data: None,
        call_bank: None,
        call_bytes: Vec::new(),
        calls: BTreeMap::new(),
        occupied: vec![false; LIMIT],
        input_names: BTreeMap::new(),
        bank_names: BTreeMap::new(),
        if_undefined: BTreeSet::new(),
        max_bss: 0x201,
    };
    let location = SourceLocation {
        file: request.input.clone(),
        line: 0,
        column: None,
    };
    if request.options.list_level > 3 {
        engine.error_at(
            &location,
            &[],
            "E_OPTIONS",
            "Listing level must be 0 through 3",
        );
        return engine.result;
    }
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
            engine.error_at(&location, &[], "E_IO", &e);
            return engine.result;
        }
    };
    let lines = match engine.load(&path, Vec::new()) {
        Ok(lines) => lines,
        Err(e) => {
            engine.error_at(&location, &[], "E_IO", &e);
            return engine.result;
        }
    };
    engine
        .input_names
        .insert(path, (1, request.input.display().to_string()));
    engine.result.binary = vec![0; LIMIT];
    // The C# port leaves the map's unassigned bytes zero-initialized.
    // Occupancy is tracked separately so new structured usage data is accurate.
    engine.result.map = vec![0; LIMIT];
    for (pass, lines) in [(Pass::Layout, lines.clone()), (Pass::Emit, lines)] {
        engine.pass = pass;
        engine.reset();
        engine.run(lines);
        if !engine.conditions.is_empty() {
            engine.error_at(&location, &[], "E_CONDITIONAL", "Missing ENDIF");
        }
        if !engine.frames.is_empty() {
            engine.error_at(&location, &[], "E_PROC", "Missing ENDP/ENDPROCGROUP");
        }
        if engine
            .result
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
        {
            break;
        }
        if pass == Pass::Layout {
            engine.relocate();
            engine.reserve("_bss_end", engine.max_bss as u32);
            engine.reserve("_nb_bank", (engine.max_bank + 1) as u32);
        }
    }
    engine.result.success = !engine
        .result
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error);
    if engine.result.success {
        let len = (engine.max_bank + 1) * BANK_SIZE;
        engine.result.binary.truncate(len);
        engine.result.map.truncate(len);
        engine.header[..4].copy_from_slice(b"NES\x1a");
        if !request.options.raw && !request.options.srec {
            engine.result.header = engine.header.to_vec();
        }
        engine.result.banks = (0..=engine.max_bank)
            .map(|bank| BankUsage {
                bank,
                used: engine.occupied[bank * BANK_SIZE..(bank + 1) * BANK_SIZE]
                    .iter()
                    .filter(|b| **b)
                    .count(),
                capacity: BANK_SIZE,
            })
            .collect();
        if engine.any_list && request.options.list_level > 0 {
            engine.result.listing = Some(engine.listing);
        }
        if request.options.srec {
            engine.result.srec = Some(crate::output::srec(
                &engine.result.binary,
                &engine.result.map,
            ));
        }
    } else {
        engine.result.binary.clear();
        engine.result.map.clear();
    }
    engine.result
}

impl Engine<'_> {
    fn reset(&mut self) {
        self.position = Position {
            page: 7,
            ..Position::default()
        };
        self.section = Section::Code;
        self.section_bank = [0; 4];
        self.saved.clear();
        self.saved
            .insert((Section::ZeroPage, 0), Position::default());
        self.saved.insert(
            (Section::Bss, 0),
            Position {
                offset: 0x200,
                ..Position::default()
            },
        );
        self.saved.insert((Section::Code, 0), self.position.clone());
        self.saved.insert((Section::Data, 0), self.position.clone());
        self.conditions.clear();
        self.frames.clear();
        self.macros.clear();
        self.functions.clear();
        self.defined.clear();
        self.if_undefined.clear();
        self.cat.clear();
        self.rs = 0;
        self.list = false;
        self.mlist = self.request.options.macro_listing;
        self.auto_zp = self.request.options.auto_zp;
        self.warn = false;
        self.line_number = 0;
        self.macro_counter = 0;
        self.last_data = None;
        if self.pass.is_emitting() {
            self.listing = format!("#[1]   {}\n", self.request.input.display());
        }
    }
    fn reserve(&mut self, name: &str, value: u32) {
        self.result.symbols.insert(
            name.into(),
            Symbol {
                name: name.into(),
                value,
                bank: RESERVED_BANK,
                page: 0,
                location: SourceLocation::default(),
                public: true,
                size: 0,
                data_type: None,
            },
        );
    }
    fn error_at(
        &mut self,
        loc: &SourceLocation,
        trace: &[SourceLocation],
        code: &str,
        message: &str,
    ) {
        self.result.diagnostics.push(Diagnostic {
            severity: Severity::Error,
            code: code.into(),
            message: message.into(),
            location: loc.clone(),
            expansion_trace: trace.into(),
        });
    }
    fn load(&mut self, path: &Path, trace: Vec<SourceLocation>) -> Result<Vec<Line>, String> {
        if !self.result.dependencies.iter().any(|p| p == path) {
            self.result.dependencies.push(path.into());
        }
        source::read_lines(self.request, path, trace)
    }
    fn active(&self) -> bool {
        self.conditions
            .last()
            .is_none_or(|c| c.parent && (c.condition != c.otherwise))
    }
    fn value(&self, text: &str) -> Result<u32, String> {
        expr::evaluate(
            text,
            &expr::Context {
                symbols: &self.result.symbols,
                regions: &self.result.regions,
                functions: &self.functions,
                global: &self.position.global,
                pc: self.pc(),
                allow_undefined: self.pass.is_layout(),
            },
        )
    }
    fn strict_value(&self, text: &str) -> Result<u32, String> {
        expr::evaluate(
            text,
            &expr::Context {
                symbols: &self.result.symbols,
                regions: &self.result.regions,
                functions: &self.functions,
                global: &self.position.global,
                pc: self.pc(),
                allow_undefined: false,
            },
        )
    }
    fn pc(&self) -> u32 {
        (self.position.page * BANK_SIZE + self.position.offset) as u32
    }
    fn linear(&self) -> usize {
        self.position.bank * BANK_SIZE + self.position.offset
    }
    fn define(
        &mut self,
        name: &str,
        value: u32,
        line: &Line,
        public: bool,
    ) -> Result<String, String> {
        let local = name.starts_with('.');
        if local && self.position.global.is_empty() {
            return Err("Local label has no global scope".into());
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
            format!("{}{name}", self.position.global)
        } else {
            name.into()
        };
        if self.pass.is_layout() && self.if_undefined.contains(&key) {
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
            return Err(format!("Symbol '{key}' is multiply defined"));
        }
        if self.pass.is_layout()
            && self
                .result
                .symbols
                .get(&key)
                .is_some_and(|s| s.location.line == 0)
        {
            return Err("Reserved symbol cannot be redefined".into());
        }
        if self.pass.is_layout() {
            self.result.symbols.insert(
                key.clone(),
                Symbol {
                    name: key.clone(),
                    value,
                    bank: if self.section.is_ram() {
                        RESERVED_BANK
                    } else {
                        self.position.bank
                    },
                    page: self.position.page,
                    location: line.location.clone(),
                    public: public || !local,
                    size: 0,
                    data_type: None,
                },
            );
            if let Some(frame) = self.frames.last() {
                self.symbol_proc.insert(key.clone(), frame.name.clone());
            }
        } else if let Some(s) = self.result.symbols.get_mut(&key) {
            if s.value != value {
                return Err(format!("Phase error for symbol '{key}'"));
            }
            s.public |= public;
        }
        if !local {
            self.position.global = key.clone();
        }
        Ok(key)
    }
    fn run(&mut self, lines: Vec<Line>) {
        let mut queue: VecDeque<PendingLine> = lines.into_iter().map(PendingLine::Source).collect();
        let mut steps = 0usize;
        while let Some(line) = self.next_source_line(&mut queue) {
            steps += 1;
            if steps > 1_000_000 {
                self.error_at(
                    &line.location,
                    &line.trace,
                    "E_LIMIT",
                    "Expanded source limit exceeded",
                );
                break;
            }
            if !line.expanded {
                self.line_number = line.location.line;
            }
            let text = source::strip_comment(&line.text);
            let (label, op, operand) = self.parse(text);
            let was_active = self.active();
            if ["IF", "IFDEF", "IFNDEF", "ELSE", "ENDIF"].contains(&op.as_str()) {
                let display = if op == "IF" {
                    self.value(&operand).ok()
                } else if op == "IFDEF" || op == "IFNDEF" {
                    Some(
                        self.result.symbols.contains_key(&operand) as u32
                            ^ u32::from(op == "IFNDEF"),
                    )
                } else {
                    None
                };
                let parent_active = self.conditions.last().is_none_or(|c| c.parent);
                if let Err(e) = self.conditional(&op, &operand) {
                    self.error_at(&line.location, &line.trace, "E_CONDITIONAL", &e);
                    break;
                }
                if self.pass.is_emitting()
                    && self.list
                    && parent_active
                    && (!line.expanded || self.mlist)
                {
                    self.list_line(&line, &self.position.clone(), &op, &[], false);
                    if let Some(value) = display {
                        self.insert_listing_value(value);
                    }
                }
                continue;
            }
            if !was_active {
                continue;
            }
            if op == "MACRO" || op == "MAC" {
                let name = label.clone().unwrap_or_else(|| operand.trim().into());
                if self.pass.is_emitting() && self.list {
                    self.list_line(&line, &self.position.clone(), "MACRO", &[], false);
                }
                let mut body = Vec::new();
                let mut found = false;
                // A macro definition must close before its source file returns.
                while let Some(PendingLine::Source(next)) = queue.pop_front() {
                    if !next.expanded {
                        self.line_number = next.location.line;
                    }
                    if self.pass.is_emitting() && self.list {
                        self.list_line(&next, &self.position.clone(), "MACRO", &[], false);
                    }
                    let (_, next_op, _) = self.parse(source::strip_comment(&next.text));
                    if next_op == "ENDM" {
                        found = true;
                        break;
                    }
                    body.push(next);
                }
                if !found
                    || name.is_empty()
                    || name.contains('.')
                    || self.macros.contains_key(&name.to_ascii_uppercase())
                {
                    self.error_at(
                        &line.location,
                        &line.trace,
                        "E_MACRO",
                        "Invalid, duplicate or unterminated macro",
                    );
                    break;
                }
                self.macros.insert(name.to_ascii_uppercase(), body);
                continue;
            }
            if let Some(body) = self.macros.get(&op).cloned() {
                if self.pass.is_emitting() && self.list && (!line.expanded || self.mlist) {
                    let mut position = self.position.clone();
                    if !self.mlist {
                        position.offset += position.page * 8192;
                    }
                    self.list_line(&line, &position, "MACRO_CALL", &[], !self.mlist);
                }
                if line.trace.len() > 32 {
                    self.error_at(
                        &line.location,
                        &line.trace,
                        "E_LIMIT",
                        "Macro nesting limit exceeded",
                    );
                    break;
                }
                if let Some(name) = label.as_deref()
                    && let Err(e) = self.define(name, self.pc(), &line, false)
                {
                    self.error_at(&line.location, &line.trace, "E_SYMBOL", &e);
                    break;
                }
                let args = match source::macro_arguments(&operand) {
                    Ok(a) => a,
                    Err(e) => {
                        self.error_at(&line.location, &line.trace, "E_MACRO", &e);
                        break;
                    }
                };
                if args.len() > 9 {
                    self.error_at(
                        &line.location,
                        &line.trace,
                        "E_MACRO",
                        "Maximum nine macro arguments",
                    );
                    break;
                }
                self.macro_counter += 1;
                let mut expanded = Vec::new();
                for mut next in body {
                    next.trace = line.trace.clone();
                    next.trace.push(line.location.clone());
                    next.expanded = true;
                    // The legacy \# loop returns 9 when the ninth argument is absent.
                    let arg_count = if args.len() < 9 { 9 } else { 0 };
                    next.text = next
                        .text
                        .replace("\\@", &format!("{:05}", self.macro_counter))
                        .replace("\\#", &arg_count.to_string());
                    for i in 1..=9 {
                        let arg = args.get(i - 1).map_or("", String::as_str);
                        let kind = if arg.is_empty() {
                            0
                        } else if arg.starts_with('#') {
                            2
                        } else if arg.starts_with('"') {
                            5
                        } else if arg.starts_with('[') {
                            4
                        } else if ["A", "X", "Y"].contains(&arg.to_ascii_uppercase().as_str()) {
                            1
                        } else if arg
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
                            && !arg.chars().next().is_some_and(|c| c.is_ascii_digit())
                        {
                            let name = if arg.starts_with('.') {
                                format!("{}{arg}", self.position.global)
                            } else {
                                arg.into()
                            };
                            if self
                                .result
                                .symbols
                                .get(&name)
                                .is_some_and(|s| s.bank == RESERVED_BANK)
                            {
                                3
                            } else {
                                6
                            }
                        } else {
                            3
                        };
                        next.text = next
                            .text
                            .replace(&format!("\\?{i}"), &kind.to_string())
                            .replace(&format!("\\{i}"), arg);
                    }
                    expanded.push(next);
                }
                for next in expanded.into_iter().rev() {
                    queue.push_front(PendingLine::Source(next));
                }
                continue;
            }
            if op == "INCLUDE" {
                let loaded = (|| -> Result<Vec<Line>, String> {
                    if let Some(name) = label.as_deref() {
                        self.define(name, self.pc(), &line, false)?;
                    }
                    let mut name = source::quoted(&operand)?;
                    if !Path::new(&name)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("asm"))
                    {
                        name.push_str(".asm");
                    }
                    let path = source::find_file(self.request, Path::new(&name))?;
                    let mut trace = line.trace.clone();
                    trace.push(line.location.clone());
                    if trace.len() > 32 {
                        return Err("Include nesting limit exceeded".into());
                    }
                    let (depth, _) = self
                        .input_names
                        .get(&line.location.file)
                        .cloned()
                        .unwrap_or((1, String::new()));
                    self.input_names
                        .insert(path.clone(), (depth + 1, name.clone()));
                    if self.pass.is_emitting()
                        && self.any_list
                        && self.request.options.list_level > 0
                    {
                        self.listing
                            .push_str(&format!("#[{}]   {name}\n", depth + 1));
                        if self.list {
                            self.list_line(&line, &self.position.clone(), "INCLUDE", &[], false);
                        }
                    }
                    self.load(&path, trace)
                })();
                match loaded {
                    Ok(lines) => {
                        let header = self
                            .input_names
                            .get(&line.location.file)
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
                        self.error_at(&line.location, &line.trace, "E_INCLUDE", &e);
                        break;
                    }
                }
                continue;
            }
            let start = self.position.clone();
            let data = self.execute(label.as_deref(), &op, &operand, &line);
            match data {
                Ok(bytes) => {
                    if self.pass.is_emitting()
                        && self.list
                        && (!line.expanded || self.mlist)
                        && !["LIST", "MLIST", "NOMLIST"].contains(&op.as_str())
                    {
                        self.list_line(&line, &start, &op, &bytes, label.is_some());
                        if (op == "=" || op == "EQU")
                            && let Ok(value) = self.value(&operand)
                        {
                            self.insert_listing_value(value);
                        }
                    }
                }
                Err(e) => {
                    self.error_at(&line.location, &line.trace, "E_ASSEMBLY", &e);
                    break;
                }
            }
        }
    }
    fn next_source_line(&mut self, queue: &mut VecDeque<PendingLine>) -> Option<Line> {
        loop {
            match queue.pop_front()? {
                PendingLine::Source(line) => return Some(line),
                PendingLine::ReturnFromInclude { depth, name } => {
                    if self.pass.is_emitting()
                        && self.any_list
                        && self.request.options.list_level > 0
                    {
                        self.listing.push_str(&format!("#[{depth}]   {name}\n"));
                    }
                }
            }
        }
    }

    fn parse(&self, text: &str) -> (Option<String>, String, String) {
        let indented = text.starts_with(char::is_whitespace);
        if text.starts_with('*') {
            return (None, String::new(), String::new());
        }
        let mut text = text.trim();
        if text.is_empty() {
            return (None, String::new(), String::new());
        }
        if let Some((name, value)) = text.split_once('=') {
            let name = name.trim();
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
            {
                return (Some(name.into()), "=".into(), value.trim().into());
            }
        }
        let mut label = None;
        let (first, rest) = word(text);
        if let Some((name, tail)) = first.split_once(':') {
            label = Some(name.into());
            text = if tail.is_empty() {
                rest
            } else {
                &text[name.len() + 1..]
            };
        } else if rest.trim_start().starts_with('=') {
            return (
                Some(first.into()),
                "=".into(),
                rest.trim_start()[1..].trim().into(),
            );
        } else if !indented
            && !is_operation(first)
            && !self.macros.contains_key(&first.to_ascii_uppercase())
        {
            label = Some(first.into());
            text = rest;
        }
        let (op, rest) = word(text);
        (
            label,
            op.trim_start_matches('.').to_ascii_uppercase(),
            rest.trim().into(),
        )
    }
    fn conditional(&mut self, op: &str, operand: &str) -> Result<(), String> {
        match op {
            "IF" | "IFDEF" | "IFNDEF" => {
                if self.conditions.len() > 64 {
                    return Err("Conditional nesting limit exceeded".into());
                }
                let parent = self.active();
                let condition = if !parent {
                    false
                } else if op == "IF" {
                    self.value(operand)? != 0
                } else {
                    let key = if operand.starts_with('.') {
                        format!("{}{operand}", self.position.global)
                    } else {
                        operand.into()
                    };
                    if self.pass.is_layout() && !self.result.symbols.contains_key(&key) {
                        self.if_undefined.insert(key.clone());
                    }
                    self.result.symbols.contains_key(&key) != (op == "IFNDEF")
                };
                self.conditions.push(Conditional {
                    parent,
                    condition,
                    otherwise: false,
                });
            }
            "ELSE" => {
                if !operand.is_empty() {
                    return Err("Unexpected ELSE operand".into());
                }
                let c = self.conditions.last_mut().ok_or("Unexpected ELSE")?;
                if c.otherwise {
                    return Err("Duplicate ELSE".into());
                }
                c.otherwise = true;
            }
            "ENDIF" => {
                if !operand.is_empty() {
                    return Err("Unexpected ENDIF operand".into());
                }
                self.conditions.pop().ok_or("Unexpected ENDIF")?;
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    fn execute(
        &mut self,
        label: Option<&str>,
        op: &str,
        operand: &str,
        line: &Line,
    ) -> Result<Vec<u8>, String> {
        if op == "PUBLIC" {
            let name = label.ok_or("PUBLIC requires a local label")?;
            if !name.starts_with('.') {
                return Err("PUBLIC requires a local label".into());
            }
            if !operand.is_empty() {
                return Err("Unexpected PUBLIC operand".into());
            }
            self.define(name, self.pc(), line, true)?;
            return Ok(Vec::new());
        }
        if op == "=" || op == "EQU" {
            let name = label.ok_or("EQU requires a label")?;
            let value = self.value(operand)?;
            let global = self.position.global.clone();
            let key = self.define(name, value, line, false)?;
            let symbol = self.result.symbols.get_mut(&key).unwrap();
            symbol.bank = RESERVED_BANK;
            symbol.page = 0;
            self.position.global = global;
            return Ok(Vec::new());
        }
        if op == "FUNC" {
            let name = label.ok_or("FUNC requires a label")?;
            if self.functions.insert(name.into(), operand.into()).is_some() {
                return Err("Duplicate function".into());
            }
            return Ok(Vec::new());
        }
        if op == "ORG" {
            let addr = self.strict_value(operand)? as usize;
            if addr > 65535 {
                return Err("ORG out of range".into());
            }
            if self.section == Section::ZeroPage && addr > 255 {
                return Err("Zero page ORG out of range".into());
            }
            if self.section == Section::Bss && addr > 0x7ff {
                return Err("BSS ORG out of range".into());
            }
            if !self.frames.is_empty() {
                return Err("ORG not allowed inside a procedure".into());
            }
            self.position.page = addr >> 13;
            self.position.offset = addr & 8191;
            if let Some(label) = label {
                self.define(label, self.pc(), line, false)?;
            }
            return Ok(Vec::new());
        }
        if op == "PROC" || op == "PROCGROUP" {
            return self.begin_proc(label, operand, line, op == "PROCGROUP");
        }
        if op == "ENDP" || op == "ENDPROCGROUP" {
            self.end_proc(op == "ENDPROCGROUP")?;
            return Ok(Vec::new());
        }
        let global = self.position.global.clone();
        let key = if let Some(name) = label {
            Some(self.define(
                name,
                if op == "RS" { self.rs } else { self.pc() },
                line,
                false,
            )?)
        } else {
            None
        };
        if op == "RS"
            && let Some(key) = &key
        {
            self.result.symbols.get_mut(key).unwrap().bank = RESERVED_BANK;
        }
        if op == "RS" {
            self.position.global = global;
        }
        let mut bytes = Vec::new();
        let start = self.position.offset;
        let line_start_bank = self.position.bank;
        match op {
            "" => {
                if label.is_none() {
                    self.last_data = None;
                }
                if !operand.is_empty() {
                    return Err("Unexpected text".into());
                }
            }
            "BANK" => {
                if self.section.is_ram() || !self.frames.is_empty() {
                    return Err("BANK not allowed in this section/procedure".into());
                }
                let args = source::arguments(operand)?;
                if args.is_empty() || args.len() > 2 {
                    return Err("BANK expects one or two arguments".into());
                }
                let bank = self.value(&args[0])? as usize;
                if bank >= 128 {
                    return Err("Bank out of range".into());
                }
                if args.len() == 2 {
                    let name = source::quoted(&args[1])?;
                    if name.len() > 63 {
                        return Err("Bank name too long".into());
                    }
                    if self
                        .bank_names
                        .get(&bank)
                        .is_some_and(|old| !old.eq_ignore_ascii_case(&name))
                    {
                        return Err("Different bank names not allowed".into());
                    }
                    self.bank_names.insert(bank, name);
                }
                self.save();
                self.position =
                    self.saved
                        .get(&(self.section, bank))
                        .cloned()
                        .unwrap_or(Position {
                            bank,
                            ..Position::default()
                        });
                self.max_bank = self.max_bank.max(bank);
            }
            "ZP" | "BSS" | "CODE" | "DATA" => {
                if !operand.is_empty() {
                    return Err("Unexpected section operand".into());
                }
                let section = match op {
                    "ZP" => Section::ZeroPage,
                    "BSS" => Section::Bss,
                    "CODE" => Section::Code,
                    _ => Section::Data,
                };
                if !self.frames.is_empty() && section != Section::Code {
                    return Err("Section not allowed in procedure".into());
                }
                self.save();
                self.section = section;
                let bank = self.section_bank[section.index()];
                self.position = self
                    .saved
                    .get(&(section, bank))
                    .cloned()
                    .unwrap_or_default();
            }
            "CATBANK" => {
                let bank = self.value(operand)? as usize;
                if bank >= 128 {
                    return Err("CATBANK out of range".into());
                }
                self.cat.insert(bank);
            }
            "BEGINREGION" | "ENDREGION" => {
                let name = source::quoted(operand)?;
                if name.is_empty() || name.len() > 64 {
                    return Err("Invalid region name".into());
                }
                if self.pass.is_layout() {
                    let address = self.linear();
                    let r = self.result.regions.entry(name.clone()).or_insert(Region {
                        name,
                        begin: None,
                        end: None,
                        size: None,
                    });
                    let slot = if op == "BEGINREGION" {
                        &mut r.begin
                    } else {
                        &mut r.end
                    };
                    if slot.is_some() {
                        return Err("Region endpoint multiply defined".into());
                    }
                    *slot = Some(address);
                    r.size = r.begin.zip(r.end).map(|(b, e)| e as i64 - b as i64);
                }
            }
            "DB" | "BYTE" | "DW" | "WORD" => {
                if self.section.is_ram() {
                    return Err("Data emission not allowed in RAM section".into());
                }
                let args = source::arguments(operand)?;
                if args.is_empty() {
                    return Err("Missing data".into());
                }
                let wide = op == "DW" || op == "WORD";
                for arg in args {
                    if !wide && arg.starts_with('"') {
                        let string = source::quoted(&arg)?;
                        let mut chars = string.chars();
                        while let Some(mut c) = chars.next() {
                            if c == '\\' {
                                c = match chars.next().ok_or("Invalid escape")? {
                                    'n' => '\n',
                                    'r' => '\r',
                                    't' => '\t',
                                    x => x,
                                };
                            }
                            bytes.push(c as u8);
                        }
                    } else {
                        let n = self.value(&arg)?;
                        if self.pass.is_emitting()
                            && (if wide {
                                n > 65535 && n < 0xffff8000
                            } else {
                                n > 255 && n < 0xffffff80
                            })
                        {
                            return Err("Data overflow".into());
                        }
                        bytes.push(n as u8);
                        if wide {
                            bytes.push((n >> 8) as u8);
                        }
                    }
                }
                self.emit(&bytes)?;
            }
            "DS" => {
                let args = source::arguments(operand)?;
                if args.is_empty() || args.len() > 2 {
                    return Err("DS expects count[, fill]".into());
                }
                let count = self.value(&args[0])? as usize;
                let fill = if args.len() == 2 {
                    self.value(&args[1])?
                } else {
                    0
                };
                if fill > 255 {
                    return Err("Fill out of range".into());
                }
                if count > LIMIT {
                    return Err("Allocation too large".into());
                }
                if self.section.is_ram() {
                    let limit = if self.section == Section::ZeroPage {
                        256
                    } else {
                        2048
                    };
                    if self.position.offset + count > limit {
                        return Err("RAM allocation out of range".into());
                    }
                    self.position.offset += count;
                    if self.section == Section::Bss {
                        self.max_bss = self.max_bss.max(self.position.offset);
                    }
                    self.save();
                } else {
                    if self.position.offset + count > 8192 {
                        return Err("DS out of range".into());
                    }
                    bytes = vec![fill as u8; count];
                    self.emit(&bytes)?;
                }
            }
            "RSSET" => {
                self.rs = self.value(operand)?;
                if self.rs > 65535 {
                    return Err("RSSET out of range".into());
                }
            }
            "RS" => {
                let n = self.value(operand)?;
                self.rs = self
                    .rs
                    .checked_add(n)
                    .filter(|n| *n <= 65535)
                    .ok_or("RS out of range")?;
            }
            "INCBIN" => {
                let args = source::arguments(operand)?;
                if args.is_empty() || args.len() > 3 {
                    return Err("INCBIN expects file[, offset[, size]]".into());
                }
                let name = source::quoted(&args[0])?;
                let path = source::find_file(self.request, Path::new(&name))?;
                if !self.result.dependencies.contains(&path) {
                    self.result.dependencies.push(path.clone());
                }
                let mut file = fs::File::open(&path).map_err(|e| e.to_string())?;
                let length = file.metadata().map_err(|e| e.to_string())?.len();
                let offset = if args.len() > 1 {
                    u64::from(self.value(&args[1])?)
                } else {
                    0
                };
                let size = if args.len() > 2 {
                    u64::from(self.value(&args[2])?)
                } else {
                    length
                        .checked_sub(offset)
                        .ok_or("INCBIN offset out of range")?
                };
                let end = offset.checked_add(size).ok_or("INCBIN range overflow")?;
                if end > length {
                    return Err("INCBIN range out of bounds".into());
                }
                let available = if self.frames.is_empty() {
                    LIMIT.checked_sub(self.linear())
                } else {
                    BANK_SIZE.checked_sub(self.position.offset)
                }
                .ok_or("ROM limit exceeded")?;
                if size > available as u64 {
                    return Err("ROM limit exceeded".into());
                }
                file.seek(SeekFrom::Start(offset))
                    .map_err(|e| e.to_string())?;
                bytes = vec![0; size as usize];
                file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
                let page = (self.position.page + (self.position.offset + bytes.len()) / 8192) & 7;
                self.emit_buffer(&bytes)?;
                self.position.page = page;
            }
            "DEFCHR" => {
                let args = source::arguments(operand)?;
                if args.len() != 8 {
                    return Err("DEFCHR expects eight rows".into());
                }
                let mut rows = [0u32; 8];
                for (row, arg) in rows.iter_mut().zip(args) {
                    *row = self.value(&arg)?;
                }
                bytes = image::packed_tile(rows, self.pass.is_emitting())?;
                self.emit_buffer(&bytes)?;
            }
            "INCCHR" => {
                let args = source::arguments(operand)?;
                if args.is_empty() {
                    return Err("INCCHR expects a PCX file".into());
                }
                let name = source::quoted(&args[0])?;
                let path = source::find_file(self.request, Path::new(&name))?;
                if !self.result.dependencies.contains(&path) {
                    self.result.dependencies.push(path.clone());
                }
                let nums = args[1..]
                    .iter()
                    .map(|a| self.value(a).map(|v| v as usize))
                    .collect::<Result<Vec<_>, _>>()?;
                bytes = image::pcx_tiles(&fs::read(path).map_err(|e| e.to_string())?, &nums)?;
                self.emit_buffer(&bytes)?;
            }
            "INESPRG" | "INESCHR" | "INESMAP" | "INESMIR" => {
                let n = self.value(operand)?;
                let limit = match op {
                    "INESPRG" => 64,
                    "INESCHR" => 64,
                    "INESMAP" => 255,
                    _ => 15,
                };
                if n > limit {
                    return Err("iNES parameter out of range".into());
                }
                match op {
                    "INESPRG" => self.header[4] = n as u8,
                    "INESCHR" => self.header[5] = n as u8,
                    "INESMAP" => {
                        self.header[6] = (self.header[6] & 15) | ((n as u8 & 15) << 4);
                        self.header[7] = (n as u8) & 0xf0;
                    }
                    _ => self.header[6] = (self.header[6] & 0xf0) | (n as u8),
                }
            }
            "AUTOZP" => {
                let n = self.value(operand)?;
                if n > 1 {
                    return Err("AUTOZP must be 0 or 1".into());
                }
                self.auto_zp = n != 0;
            }
            "LIST" | "NOLIST" | "MLIST" | "NOMLIST" => {
                if !operand.is_empty() {
                    return Err("Unexpected listing operand".into());
                }
                match op {
                    "LIST" => {
                        self.list = true;
                        self.any_list = true;
                    }
                    "NOLIST" => self.list = false,
                    "MLIST" => self.mlist = true,
                    _ => self.mlist = self.request.options.macro_listing,
                }
            }
            "OPT" => {
                for opt in source::arguments(operand)? {
                    let opt = opt.to_ascii_lowercase();
                    let flag = opt.ends_with('+');
                    if !flag && !opt.ends_with('-') {
                        return Err("Invalid OPT flag".into());
                    }
                    match &opt[..opt.len() - 1] {
                        "l" => {
                            self.list = flag;
                            self.any_list |= flag;
                        }
                        "m" => self.mlist = flag || self.request.options.macro_listing,
                        "w" => self.warn = flag,
                        "o" => {}
                        _ => return Err("Unknown OPT".into()),
                    }
                }
            }
            "FAIL" => {
                return Err(if operand.is_empty() {
                    "Assembly failed".into()
                } else {
                    operand.into()
                });
            }
            "ENDM" => return Err("Unexpected ENDM".into()),
            "CALL" => {
                bytes = self.call(operand)?;
                self.emit(&bytes)?;
            }
            _ => {
                if self.section.is_ram() {
                    return Err("Instruction not allowed in RAM section".into());
                }
                bytes = self.instruction(op, operand)?;
                self.emit(&bytes)?;
            }
        }
        if self.pass.is_emitting()
            && self.warn
            && !self.request.options.warning_disabled
            && ["INCBIN", "INCCHR"].contains(&op)
            && self.position.bank > line_start_bank
        {
            let overflow = (self.position.bank - line_start_bank - 1) * 8192 + self.position.offset;
            if overflow > 0 {
                self.result.diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    code: "W_BANK_OVERFLOW".into(),
                    message: format!("Bank overflow by {overflow} bytes"),
                    location: line.location.clone(),
                    expansion_trace: line.trace.clone(),
                });
            }
        }
        if ["DB", "BYTE", "DW", "WORD", "INCBIN", "INCCHR"].contains(&op) {
            let kind = match op {
                "INCBIN" => DataType::Binary,
                "INCCHR" => DataType::Characters,
                _ => DataType::Bytes,
            };
            let amount = if self.position.offset >= start {
                self.position.offset - start
            } else {
                bytes.len()
            };
            if let Some(key) = key {
                if let Some(s) = self.result.symbols.get_mut(&key) {
                    s.size = amount;
                    s.data_type = Some(kind);
                }
                self.last_data = Some((key, kind));
            } else if let Some((name, previous)) = &self.last_data
                && *previous == kind
                && let Some(s) = self.result.symbols.get_mut(name)
            {
                s.size += amount;
            }
        } else if !op.is_empty() {
            self.last_data = None;
        }
        Ok(bytes)
    }
    fn save(&mut self) {
        self.saved
            .insert((self.section, self.position.bank), self.position.clone());
        self.section_bank[self.section.index()] = self.position.bank;
    }
    fn emit(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.emit_impl(bytes, false)
    }
    fn emit_buffer(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.emit_impl(bytes, true)
    }
    fn emit_impl(&mut self, bytes: &[u8], buffer: bool) -> Result<(), String> {
        if self.section.is_ram() {
            return Err("Data emission not allowed in RAM section".into());
        }
        let mut pos = self.position.clone();
        for byte in bytes {
            if pos.offset >= 8192 {
                if !self.frames.is_empty() {
                    return Err("Procedure exceeds 8 KiB".into());
                }
                if !buffer && !self.cat.contains(&pos.bank) {
                    return Err("Bank overflow, offset > $1FFF".into());
                }
                pos.bank += 1;
                pos.page = (pos.page + 1) & 7;
                if buffer && pos.page == 0 {
                    pos.page = 4;
                }
                pos.offset = 0;
            }
            let address = pos.bank * BANK_SIZE + pos.offset;
            if address >= LIMIT && self.pass.is_emitting() {
                return Err("ROM limit exceeded".into());
            }
            if pos.bank < 128 {
                self.max_bank = self.max_bank.max(pos.bank);
            }
            if self.pass.is_emitting() {
                self.result.binary[address] = *byte;
                self.result.map[address] = self.section.map_byte(pos.page);
                self.occupied[address] = true;
            }
            pos.offset += 1;
        }
        self.position = pos;
        if self.position.offset == 8192 && (buffer || self.cat.contains(&self.position.bank)) {
            self.position.bank += 1;
            self.position.page = (self.position.page + 1) & 7;
            self.position.offset = 0;
            if buffer && self.position.page == 0 {
                self.position.page = 4;
            }
        }
        Ok(())
    }
    fn instruction(&self, op: &str, operand: &str) -> Result<Vec<u8>, String> {
        let (name, extension) = op.split_once('.').map_or((op, None), |(n, e)| (n, Some(e)));
        if extension.is_some() {
            return Err("Unknown instruction extension; use low_byte/high_byte".into());
        }
        if opcode::opcode(name, Mode::Imp).is_some() {
            if !operand.is_empty() {
                return Err("Unexpected operand".into());
            }
            return Ok(vec![opcode::opcode(name, Mode::Imp).unwrap()]);
        }
        if let Some(code) = opcode::opcode(name, Mode::Rel) {
            let value = self.value(operand)?;
            let delta = value.wrapping_sub(self.pc() + 2) as i32;
            if self.pass.is_emitting() && !(-128..=127).contains(&delta) {
                return Err("Branch address out of range".into());
            }
            return Ok(vec![code, delta as u8]);
        }
        let mut compact = operand.trim();
        let ext = if compact.to_ascii_lowercase().starts_with("low_byte ") {
            compact = compact[9..].trim_start();
            Some("L")
        } else if compact.to_ascii_lowercase().starts_with("high_byte ") {
            compact = compact[10..].trim_start();
            Some("H")
        } else {
            None
        };
        if compact.eq_ignore_ascii_case("A") {
            return opcode::opcode(name, Mode::Acc)
                .map(|c| vec![c])
                .ok_or("Invalid accumulator mode".into());
        }
        let mut expr = compact;
        let mut mode;
        let mut auto_increment = None;
        let mut auto_tag = None;
        if let Some(rest) = compact.strip_prefix('#') {
            expr = rest;
            mode = Mode::Imm;
        } else {
            let upper = compact.to_ascii_uppercase().replace(' ', "");
            let brackets = compact.starts_with('[');
            let parens = compact.starts_with('(') && (self.auto_zp || name == "JMP");
            if brackets || parens {
                let close = if brackets { ']' } else { ')' };
                let index = compact.rfind(close).ok_or("Missing indirect delimiter")?;
                let inner = &compact[1..index];
                let tail = compact[index + 1..].trim().to_ascii_uppercase();
                let parts = source::arguments(inner)?;
                if parts.len() == 2 && parts[1].eq_ignore_ascii_case("X") {
                    expr = &inner[..inner.rfind(',').unwrap()];
                    mode = if name == "JMP" { Mode::Ix } else { Mode::Zix };
                } else if let Some(tag) = tail.strip_prefix('.') {
                    expr = inner;
                    mode = Mode::Ziy;
                    auto_tag = Some(self.value(tag)? as u8);
                } else if tail.starts_with(",Y") {
                    expr = inner;
                    mode = Mode::Ziy;
                    if tail == ",Y++" {
                        auto_increment = Some(0xc8);
                    } else if tail != ",Y" {
                        return Err("Invalid indirect operand".into());
                    }
                } else if !tail.is_empty() {
                    return Err("Invalid indirect operand".into());
                } else {
                    expr = inner;
                    mode = if name == "JMP" { Mode::Ind } else { Mode::Zi };
                }
            } else if upper.ends_with(",X")
                || upper.ends_with(",Y")
                || upper.ends_with(",X++")
                || upper.ends_with(",Y++")
            {
                let comma = compact.rfind(',').unwrap();
                expr = compact[..comma].trim();
                let suffix = compact[comma + 1..].trim().to_ascii_uppercase();
                mode = if suffix.starts_with('X') {
                    Mode::Ax
                } else {
                    Mode::Ay
                };
                if suffix.ends_with('+') {
                    auto_increment = Some(if mode == Mode::Ax { 0xe8 } else { 0xc8 });
                }
            } else {
                mode = Mode::Abs;
            }
        }
        let forced = expr.trim().starts_with('<');
        let absolute = expr.trim().starts_with('>');
        expr = expr.trim().trim_start_matches(['<', '>']);
        let mut value = self.value(expr)?;
        if forced || (self.auto_zp && !absolute && value <= 255) {
            let zp = match mode {
                Mode::Abs => Mode::Zp,
                Mode::Ax => Mode::Zpx,
                Mode::Ay => Mode::Zpy,
                m => m,
            };
            if opcode::opcode(name, zp).is_some() {
                mode = zp;
            } else if forced {
                return Err("Invalid zero page mode".into());
            }
        }
        let code = opcode::opcode(name, mode)
            .ok_or_else(|| format!("Unknown instruction or addressing mode '{op}'"))?;
        let wide = matches!(mode, Mode::Abs | Mode::Ax | Mode::Ay | Mode::Ind | Mode::Ix);
        if let Some(ext) = ext {
            if mode == Mode::Imm {
                value = if ext == "L" {
                    value & 255
                } else {
                    (value >> 8) & 255
                };
            } else if auto_increment.is_none() {
                if matches!(
                    mode,
                    Mode::Zi | Mode::Zix | Mode::Ziy | Mode::Ind | Mode::Ix
                ) {
                    return Err("Instruction extension not supported in indirect modes".into());
                }
                if ext == "H" {
                    value = value.wrapping_add(1);
                }
            }
        }
        if self.pass.is_emitting()
            && (if wide {
                value > 65535
            } else if mode == Mode::Imm {
                value > 255 && value < 0xffffff00
            } else {
                value > 255
            })
        {
            return Err("Operand size error".into());
        }
        let mut bytes = Vec::new();
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
    fn begin_proc(
        &mut self,
        label: Option<&str>,
        operand: &str,
        line: &Line,
        group: bool,
    ) -> Result<Vec<u8>, String> {
        if self.section != Section::Code {
            return Err("Procedure requires CODE section".into());
        }
        if !self.frames.is_empty() && (group || !self.frames.last().unwrap().group) {
            return Err("Cannot nest procedures/groups".into());
        }
        let name = label.unwrap_or(operand).trim().to_string();
        let name = if name.is_empty() && group {
            format!("__group_{}__", self.procedures.len() + 1)
        } else {
            name
        };
        if name.is_empty() || name.starts_with('.') {
            return Err("Invalid procedure name".into());
        }
        let saved = self.position.clone();
        if self.pass.is_layout() {
            if self.procedures.iter().any(|p| p.name == name) {
                return Err("Duplicate procedure".into());
            }
            let base = if self.frames.is_empty() {
                0
            } else {
                self.position.offset
            };
            let parent = self.frames.last().map(|f| f.name.clone());
            self.procedures.push(Procedure {
                name: name.clone(),
                base,
                org: base,
                bank: PROCEDURE_BANK,
                size: 0,
                group: parent,
            });
        }
        let p = self
            .procedures
            .iter()
            .find(|p| p.name == name)
            .ok_or("Procedure not found")?;
        self.position = Position {
            bank: p.bank,
            page: 5,
            offset: p.org,
            global: name.clone(),
        };
        self.frames.push(ProcFrame {
            name: name.clone(),
            saved,
            group,
        });
        self.define(&name, self.pc(), line, false)?;
        Ok(Vec::new())
    }
    fn end_proc(&mut self, group: bool) -> Result<(), String> {
        let frame = self.frames.pop().ok_or("Unexpected procedure end")?;
        if frame.group != group {
            return Err("Mismatched procedure end".into());
        }
        let end = self.position.offset;
        if self.pass.is_layout() {
            let p = self
                .procedures
                .iter_mut()
                .find(|p| p.name == frame.name)
                .unwrap();
            p.size = end - p.base;
            if p.size > 8192 {
                return Err("Procedure too large".into());
            }
        }
        self.position = frame.saved;
        if !self.frames.is_empty() {
            self.position.offset = end;
        }
        Ok(())
    }
    fn relocate(&mut self) {
        if self.procedures.is_empty() {
            return;
        }
        let mut bank = self.max_bank + 1;
        let mut offset = 0;
        for i in 0..self.procedures.len() {
            let p = self.procedures[i].clone();
            if let Some(parent) = &p.group {
                let parent = self
                    .procedures
                    .iter()
                    .find(|g| &g.name == parent)
                    .unwrap()
                    .clone();
                self.procedures[i].org = p.base + parent.org - parent.base;
                self.procedures[i].bank = parent.bank;
            } else {
                if offset + p.size > 8192 {
                    bank += 1;
                    offset = 0;
                }
                self.procedures[i].bank = bank;
                self.procedures[i].org = offset;
                offset += p.size;
            }
        }
        if bank >= 128 {
            self.error_at(
                &SourceLocation::default(),
                &[],
                "E_PROC",
                "Not enough ROM space for procedures",
            );
            return;
        }
        self.max_bank = bank;
        for (name, proc_name) in &self.symbol_proc {
            let p = self
                .procedures
                .iter()
                .find(|p| &p.name == proc_name)
                .unwrap();
            if let Some(s) = self.result.symbols.get_mut(name) {
                s.value = s
                    .value
                    .wrapping_add(p.org as u32)
                    .wrapping_sub(p.base as u32);
                s.bank = p.bank;
            }
        }
        self.reserve("_call_bank", (bank + 1) as u32);
    }
    fn call(&mut self, name: &str) -> Result<Vec<u8>, String> {
        if self.pass.is_layout() {
            return Ok(vec![0x20, 0, 0]);
        }
        let target = if let Some(p) = self.procedures.iter().find(|p| p.name == name).cloned() {
            if self.position.bank == p.bank {
                (0xa000 + p.org) as u32
            } else if let Some(addr) = self.calls.get(name) {
                *addr
            } else {
                let bank = if let Some(b) = self.call_bank {
                    b
                } else {
                    let b = self.max_bank + 1;
                    if b >= 128 {
                        return Err("Call bank exceeds ROM limit".into());
                    }
                    self.call_bank = Some(b);
                    self.max_bank = b;
                    b
                };
                let offset = self.call_bytes.len();
                if offset + 18 > 8192 {
                    return Err("Call bank overflow".into());
                }
                let stub = [
                    0xa8,
                    0x43,
                    0x20,
                    0x48,
                    0xa9,
                    p.bank as u8,
                    0x53,
                    0x20,
                    0x98,
                    0x20,
                    p.org as u8,
                    ((p.org >> 8) + 0xa0) as u8,
                    0xa8,
                    0x68,
                    0x53,
                    0x20,
                    0x98,
                    0x60,
                ];
                for (i, b) in stub.iter().enumerate() {
                    self.result.binary[bank * 8192 + offset + i] = *b;
                    self.result.map[bank * 8192 + offset + i] = Section::Code.map_byte(4);
                    self.occupied[bank * 8192 + offset + i] = true;
                }
                self.call_bytes.extend(stub);
                let addr = (0x8000 + offset) as u32;
                self.calls.insert(name.into(), addr);
                addr
            }
        } else {
            self.value(name)?
        };
        Ok(vec![0x20, target as u8, (target >> 8) as u8])
    }
    fn insert_listing_value(&mut self, value: u32) {
        let start = self.listing[..self.listing.len() - 1]
            .rfind('\n')
            .map_or(0, |i| i + 1);
        self.listing.replace_range(start + 7..start + 14, "       ");
        self.listing
            .replace_range(start + 16..start + 20, &format!("{:04X}", value & 65535));
    }
    fn list_line(
        &mut self,
        line: &Line,
        start: &Position,
        op: &str,
        bytes: &[u8],
        has_label: bool,
    ) {
        if self.request.options.list_level == 0 {
            return;
        }
        let level = if op == "DEFCHR" {
            3
        } else if ["DB", "BYTE", "DW", "WORD"].contains(&op) {
            2
        } else {
            0
        };
        let width = if op == "DW" || op == "WORD" { 2 } else { 3 };
        let chunks =
            if bytes.is_empty() || (level > self.request.options.list_level && bytes.len() > 3) {
                vec![&[][..]]
            } else {
                bytes.chunks(width).collect::<Vec<_>>()
            };
        for (i, chunk) in chunks.iter().enumerate() {
            let mut prefix = vec![' '; 26];
            if i == 0 && !line.expanded {
                put(&mut prefix, 0, &format!("{:5}", self.line_number));
            }
            let bank = if start.bank < 128 {
                format!("{:02X}", start.bank)
            } else {
                "--".into()
            };
            if !bytes.is_empty() || has_label || op == "PROC" || op == "PROCGROUP" {
                let address = format!("{:04X}", start.page * 8192 + start.offset + i * width);
                put(&mut prefix, 7, &format!("{bank}:{}", &address[..4]));
            } else if ["BANK", "ORG", "ZP", "BSS", "CODE", "DATA", "RSSET"].contains(&op) {
                let value = match op {
                    "BANK" => self.position.bank,
                    "RSSET" => self.rs as usize,
                    _ => self.pc() as usize,
                };
                put(&mut prefix, 16, &format!("{value:04X}"));
            }
            for (j, b) in chunk.iter().enumerate() {
                put(&mut prefix, 16 + j * 3, &format!("{b:02X}"));
            }
            self.listing.extend(prefix);
            if i == 0 {
                self.listing.push_str(&line.text);
            }
            self.listing.push('\n');
        }
    }
}
fn put(buffer: &mut [char], offset: usize, text: &str) {
    for (out, c) in buffer[offset..].iter_mut().zip(text.chars()) {
        *out = c;
    }
}
fn word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    text.split_once(char::is_whitespace)
        .map_or((text, ""), |(a, b)| (a, b.trim_start()))
}
fn is_operation(op: &str) -> bool {
    let op = op.trim_start_matches('.').to_ascii_uppercase();
    let base = op.split('.').next().unwrap_or("");
    opcode::known(base)
        || [
            "=",
            "BANK",
            "BSS",
            "BYTE",
            "CALL",
            "CODE",
            "DATA",
            "DB",
            "DW",
            "DS",
            "ELSE",
            "ENDIF",
            "ENDM",
            "ENDP",
            "ENDPROCGROUP",
            "EQU",
            "FAIL",
            "FUNC",
            "IF",
            "IFDEF",
            "IFNDEF",
            "INCBIN",
            "INCLUDE",
            "INCCHR",
            "LIST",
            "MAC",
            "MACRO",
            "MLIST",
            "NOLIST",
            "NOMLIST",
            "OPT",
            "ORG",
            "PROC",
            "PROCGROUP",
            "RSSET",
            "RS",
            "WORD",
            "ZP",
            "CATBANK",
            "BEGINREGION",
            "ENDREGION",
            "PUBLIC",
            "DEFCHR",
            "INESPRG",
            "INESCHR",
            "INESMAP",
            "INESMIR",
            "AUTOZP",
        ]
        .contains(&base)
}

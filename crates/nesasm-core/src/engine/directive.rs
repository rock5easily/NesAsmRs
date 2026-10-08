//! Pseudo-instructions other than macros, includes and conditionals.

use super::{Cursor, Engine, Position, Statement};
use crate::error::{AsmError, AsmResult};
use crate::source::{self, Line};
use crate::state::{BANK_SIZE, BSS_LIMIT, MAX_BANKS, PCX_LIMIT, ROM_LIMIT, Section, ZP_LIMIT};
use crate::{DataType, Diagnostic, DiagnosticCode, Region, Severity, SourceEncoding, image};
use std::{path::Path, rc::Rc};

/// Directive names after upper-casing and removing a leading dot; aliases
/// (`DB`/`BYTE`, `MACRO`/`MAC`, ...) map to one variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Directive {
    If,
    Ifdef,
    Ifndef,
    Else,
    Endif,
    Macro,
    Endm,
    Include,
    Equ,
    Func,
    Org,
    Page,
    Proc,
    Procgroup,
    Endp,
    Endprocgroup,
    Public,
    Bank,
    Zp,
    Bss,
    Code,
    Data,
    Catbank,
    Beginregion,
    Endregion,
    Db,
    Dw,
    Ds,
    Rsset,
    Rs,
    Incbin,
    Defchr,
    Incchr,
    Inesprg,
    Ineschr,
    Inesmap,
    Inesmir,
    Autozp,
    List,
    Nolist,
    Mlist,
    Nomlist,
    Opt,
    Fail,
    Call,
}

impl Directive {
    pub fn parse(op: &str) -> Option<Self> {
        Some(match op {
            "IF" => Self::If,
            "IFDEF" => Self::Ifdef,
            "IFNDEF" => Self::Ifndef,
            "ELSE" => Self::Else,
            "ENDIF" => Self::Endif,
            "MACRO" | "MAC" => Self::Macro,
            "ENDM" => Self::Endm,
            "INCLUDE" => Self::Include,
            "=" | "EQU" => Self::Equ,
            "FUNC" => Self::Func,
            "ORG" => Self::Org,
            "PAGE" => Self::Page,
            "PROC" => Self::Proc,
            "PROCGROUP" => Self::Procgroup,
            "ENDP" => Self::Endp,
            "ENDPROCGROUP" => Self::Endprocgroup,
            "PUBLIC" => Self::Public,
            "BANK" => Self::Bank,
            "ZP" => Self::Zp,
            "BSS" => Self::Bss,
            "CODE" => Self::Code,
            "DATA" => Self::Data,
            "CATBANK" => Self::Catbank,
            "BEGINREGION" => Self::Beginregion,
            "ENDREGION" => Self::Endregion,
            "DB" | "BYTE" => Self::Db,
            "DW" | "WORD" => Self::Dw,
            "DS" => Self::Ds,
            "RSSET" => Self::Rsset,
            "RS" => Self::Rs,
            "INCBIN" => Self::Incbin,
            "DEFCHR" => Self::Defchr,
            "INCCHR" => Self::Incchr,
            "INESPRG" => Self::Inesprg,
            "INESCHR" => Self::Ineschr,
            "INESMAP" => Self::Inesmap,
            "INESMIR" => Self::Inesmir,
            "AUTOZP" => Self::Autozp,
            "LIST" => Self::List,
            "NOLIST" => Self::Nolist,
            "MLIST" => Self::Mlist,
            "NOMLIST" => Self::Nomlist,
            "OPT" => Self::Opt,
            "FAIL" => Self::Fail,
            "CALL" => Self::Call,
            _ => return None,
        })
    }

    pub const fn is_conditional(self) -> bool {
        matches!(
            self,
            Self::If | Self::Ifdef | Self::Ifndef | Self::Else | Self::Endif
        )
    }

    /// The kind of data a labelled line produces, for SIZEOF.
    const fn data_type(self) -> Option<DataType> {
        match self {
            Self::Db | Self::Dw => Some(DataType::Bytes),
            Self::Incbin => Some(DataType::Binary),
            Self::Incchr => Some(DataType::Characters),
            _ => None,
        }
    }
}

impl Engine<'_> {
    /// Executes one directive or instruction line and returns the bytes it
    /// produced (for the listing). The label is defined here.
    pub(super) fn execute(
        &mut self,
        statement: &Statement,
        directive: Option<Directive>,
        line: &Line,
    ) -> AsmResult<Vec<u8>> {
        let label = statement.label.as_deref();
        let operand = statement.operand.as_str();
        // Directives that define their label themselves.
        match directive {
            Some(Directive::Public) => {
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
            Some(Directive::Equ) => {
                let name = label.ok_or("EQU requires a label")?;
                let value = self.value(operand)?;
                self.define_constant(name, value, line)?;
                return Ok(Vec::new());
            }
            Some(Directive::Func) => {
                let name = label.ok_or("FUNC requires a label")?;
                if self.functions.insert(name.into(), operand.into()).is_some() {
                    return Err("Duplicate function".into());
                }
                return Ok(Vec::new());
            }
            Some(Directive::Org) => {
                self.org(operand)?;
                if let Some(label) = label {
                    self.define(label, self.pc(), line, false)?;
                }
                return Ok(Vec::new());
            }
            Some(Directive::Page) => {
                if self.procs.inside() {
                    return Err(AsmError::fatal("PAGE can not be changed in procs"));
                }
                if let Some(label) = label {
                    self.define(label, self.pc(), line, false)?;
                }
                let page = self.value(operand)? as usize;
                if page > 7 {
                    return Err("Invalid page index".into());
                }
                self.position.page = page;
                return Ok(Vec::new());
            }
            Some(Directive::Proc | Directive::Procgroup) => {
                self.begin_proc(
                    label,
                    operand,
                    line,
                    directive == Some(Directive::Procgroup),
                )?;
                return Ok(Vec::new());
            }
            Some(Directive::Endp | Directive::Endprocgroup) => {
                self.end_proc(directive == Some(Directive::Endprocgroup))?;
                return Ok(Vec::new());
            }
            _ => {}
        }
        let key = match label {
            Some(name) if directive == Some(Directive::Rs) => {
                Some(self.define_constant(name, self.rs, line)?)
            }
            Some(name) => Some(self.define(name, self.pc(), line, false)?),
            None => None,
        };
        let start = self.position.offset;
        let start_bank = self.position.bank;
        let mut bytes = Vec::new();
        // Bytes produced, including INCBIN data the layout pass does not read.
        let mut produced = 0;
        match directive {
            None if statement.op.is_empty() => {
                if label.is_none() {
                    self.last_data = None;
                }
                if !operand.is_empty() {
                    return Err("Unexpected text".into());
                }
            }
            None => {
                if self.section.is_ram() {
                    return Err("Instruction not allowed in RAM section".into());
                }
                bytes = self.instruction(&statement.op, operand)?;
                self.emit(&bytes)?;
            }
            Some(Directive::Bank) => self.bank(operand)?,
            Some(Directive::Zp) => self.switch_section(Section::ZeroPage, operand)?,
            Some(Directive::Bss) => self.switch_section(Section::Bss, operand)?,
            Some(Directive::Code) => self.switch_section(Section::Code, operand)?,
            Some(Directive::Data) => self.switch_section(Section::Data, operand)?,
            Some(Directive::Catbank) => {
                let bank = self.value(operand)? as usize;
                if bank >= MAX_BANKS {
                    return Err("CATBANK out of range".into());
                }
                self.cat.insert(bank);
            }
            Some(d @ (Directive::Beginregion | Directive::Endregion)) => {
                self.region(operand, d == Directive::Beginregion)?;
            }
            Some(d @ (Directive::Db | Directive::Dw)) => {
                bytes = self.data(operand, d == Directive::Dw)?;
                self.emit(&bytes)?;
                produced = bytes.len();
            }
            Some(Directive::Ds) => {
                bytes = self.reserve_space(operand)?;
                produced = bytes.len();
            }
            Some(Directive::Rsset) => {
                self.rs = self.value(operand)?;
                if self.rs > 65535 {
                    return Err("RSSET out of range".into());
                }
            }
            Some(Directive::Rs) => {
                let n = self.value(operand)?;
                self.rs = self
                    .rs
                    .checked_add(n)
                    .filter(|n| *n <= 65535)
                    .ok_or("RS out of range")?;
            }
            Some(Directive::Incbin) => {
                (bytes, produced) = self.incbin(operand)?;
            }
            Some(Directive::Defchr) => {
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
            Some(Directive::Incchr) => {
                bytes = self.incchr(operand)?.to_vec();
                self.emit_buffer(&bytes)?;
                produced = bytes.len();
            }
            Some(
                d @ (Directive::Inesprg
                | Directive::Ineschr
                | Directive::Inesmap
                | Directive::Inesmir),
            ) => self.ines(d, operand)?,
            Some(Directive::Autozp) => {
                let n = self.value(operand)?;
                if n > 1 {
                    return Err("AUTOZP must be 0 or 1".into());
                }
                self.auto_zp = n != 0;
            }
            Some(
                d @ (Directive::List | Directive::Nolist | Directive::Mlist | Directive::Nomlist),
            ) => {
                if !operand.is_empty() {
                    return Err("Unexpected listing operand".into());
                }
                match d {
                    Directive::List => {
                        self.listing.enabled = true;
                        self.listing.requested = true;
                    }
                    Directive::Nolist => self.listing.enabled = false,
                    Directive::Mlist => self.listing.macros = true,
                    _ => self.listing.macros = self.request.options.macro_listing,
                }
            }
            Some(Directive::Opt) => self.opt(operand)?,
            Some(Directive::Fail) => {
                return Err(if operand.is_empty() {
                    "Assembly failed".into()
                } else {
                    operand.into()
                });
            }
            Some(Directive::Endm) => return Err("Unexpected ENDM".into()),
            Some(Directive::Call) => {
                bytes = self.call(operand)?;
                self.emit(&bytes)?;
            }
            Some(
                Directive::If
                | Directive::Ifdef
                | Directive::Ifndef
                | Directive::Else
                | Directive::Endif
                | Directive::Macro
                | Directive::Include
                | Directive::Equ
                | Directive::Func
                | Directive::Org
                | Directive::Page
                | Directive::Proc
                | Directive::Procgroup
                | Directive::Endp
                | Directive::Endprocgroup
                | Directive::Public,
            ) => unreachable!("handled before execute"),
        }
        if matches!(directive, Some(Directive::Incbin | Directive::Incchr)) {
            self.warn_bank_overflow(start_bank, line);
        }
        match directive.and_then(Directive::data_type) {
            Some(kind) => {
                let amount = if self.position.offset >= start {
                    self.position.offset - start
                } else {
                    produced
                };
                self.record_data_size(key, kind, amount);
            }
            None if !statement.op.is_empty() => self.last_data = None,
            None => {}
        }
        Ok(bytes)
    }

    /// Tracks SIZEOF: a labelled data line starts a block that following
    /// unlabelled lines of the same kind extend.
    fn record_data_size(&mut self, key: Option<String>, kind: DataType, amount: usize) {
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
    }

    fn warn_bank_overflow(&mut self, start_bank: usize, line: &Line) {
        if self.pass.is_emitting()
            && self.warn
            && !self.request.options.warning_disabled
            && self.position.bank > start_bank
        {
            let overflow = (self.position.bank - start_bank - 1) * BANK_SIZE + self.position.offset;
            if overflow > 0 {
                self.result.diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    code: DiagnosticCode::BankOverflow,
                    message: format!("Bank overflow by {overflow} bytes"),
                    location: line.location(),
                    expansion_trace: line.trace.to_vec(),
                });
            }
        }
    }

    fn org(&mut self, operand: &str) -> AsmResult<()> {
        let addr = self.strict_value(operand)? as usize;
        if addr > 65535 {
            return Err("ORG out of range".into());
        }
        if self.section == Section::ZeroPage && addr >= ZP_LIMIT {
            return Err("Zero page ORG out of range".into());
        }
        if self.section == Section::Bss && addr >= BSS_LIMIT {
            return Err("BSS ORG out of range".into());
        }
        if self.procs.inside() {
            return Err("ORG not allowed inside a procedure".into());
        }
        self.position.page = addr / BANK_SIZE;
        self.position.offset = addr % BANK_SIZE;
        Ok(())
    }

    /// Saves the cursor of the current section and bank.
    fn save(&mut self) {
        self.saved.insert(
            (self.section, self.position.bank),
            Cursor {
                position: self.position,
                scope: Rc::clone(&self.scope),
            },
        );
        self.section_bank[self.section.index()] = self.position.bank;
    }

    fn restore(&mut self, section: Section, bank: usize) {
        let cursor = self.saved.get(&(section, bank)).cloned().unwrap_or(Cursor {
            position: Position {
                bank,
                ..Position::default()
            },
            scope: Rc::from(""),
        });
        self.position = cursor.position;
        self.scope = cursor.scope;
    }

    fn bank(&mut self, operand: &str) -> AsmResult<()> {
        if self.section.is_ram() || self.procs.inside() {
            return Err("BANK not allowed in this section/procedure".into());
        }
        let args = source::arguments(operand)?;
        if args.is_empty() || args.len() > 2 {
            return Err("BANK expects one or two arguments".into());
        }
        let bank = self.value(&args[0])? as usize;
        if bank >= MAX_BANKS {
            return Err("Bank out of range".into());
        }
        if let Some(name) = args.get(1) {
            let name = source::quoted(name)?;
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
        self.restore(self.section, bank);
        self.rom.max_bank = self.rom.max_bank.max(bank);
        Ok(())
    }

    fn switch_section(&mut self, section: Section, operand: &str) -> AsmResult<()> {
        if !operand.is_empty() {
            return Err("Unexpected section operand".into());
        }
        if self.procs.inside() && section != Section::Code {
            return Err("Section not allowed in procedure".into());
        }
        self.save();
        self.section = section;
        let bank = self.section_bank[section.index()];
        // A section never used in this bank starts at bank 0, offset 0.
        let cursor = self.saved.get(&(section, bank)).cloned();
        match cursor {
            Some(cursor) => {
                self.position = cursor.position;
                self.scope = cursor.scope;
            }
            None => {
                self.position = Position::default();
                self.scope = Rc::from("");
            }
        }
        Ok(())
    }

    fn region(&mut self, operand: &str, begin: bool) -> AsmResult<()> {
        let name = source::quoted(operand)?;
        if name.is_empty() || name.len() > 64 {
            return Err("Invalid region name".into());
        }
        if self.pass.is_layout() {
            let address = self.position.linear();
            let r = self.result.regions.entry(name.clone()).or_insert(Region {
                name,
                begin: None,
                end: None,
                size: None,
            });
            let slot = if begin { &mut r.begin } else { &mut r.end };
            if slot.is_some() {
                return Err("Region endpoint multiply defined".into());
            }
            *slot = Some(address);
            r.size = r.begin.zip(r.end).map(|(b, e)| e as i64 - b as i64);
        }
        Ok(())
    }

    /// Appends a string character as it is encoded in the source file: ASCII as
    /// one byte, other characters as their UTF-8 or SJIS bytes. (C# keeps only
    /// the low byte of the character code.)
    fn push_char(&self, bytes: &mut Vec<u8>, c: char) -> AsmResult<()> {
        if c.is_ascii() {
            bytes.push(c as u8);
            return Ok(());
        }
        let mut buffer = [0; 4];
        let text = c.encode_utf8(&mut buffer);
        match self.request.options.encoding {
            SourceEncoding::Utf8 => bytes.extend_from_slice(text.as_bytes()),
            SourceEncoding::Sjis => {
                let (encoded, _, unmappable) = encoding_rs::SHIFT_JIS.encode(text);
                if unmappable {
                    return Err(format!("Character '{c}' cannot be encoded as SJIS").into());
                }
                bytes.extend_from_slice(&encoded);
            }
        }
        Ok(())
    }

    /// DB/DW operands: numbers and (for DB) strings with `\` escapes.
    fn data(&mut self, operand: &str, wide: bool) -> AsmResult<Vec<u8>> {
        if self.section.is_ram() {
            return Err("Data emission not allowed in RAM section".into());
        }
        let args = source::arguments(operand)?;
        if args.is_empty() {
            return Err("Missing data".into());
        }
        let mut bytes = Vec::new();
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
                    self.push_char(&mut bytes, c)?;
                }
            } else {
                let n = self.value(&arg)?;
                let overflow = if wide {
                    n > 65535 && n < 0xffff_8000
                } else {
                    n > 255 && n < 0xffff_ff80
                };
                if self.pass.is_emitting() && overflow {
                    return Err("Data overflow".into());
                }
                bytes.push(n as u8);
                if wide {
                    bytes.push((n >> 8) as u8);
                }
            }
        }
        Ok(bytes)
    }

    /// DS: reserves RAM, or fills ROM space.
    fn reserve_space(&mut self, operand: &str) -> AsmResult<Vec<u8>> {
        let args = source::arguments(operand)?;
        if args.is_empty() || args.len() > 2 {
            return Err("DS expects count[, fill]".into());
        }
        let count = self.value(&args[0])? as usize;
        let fill = match args.get(1) {
            Some(fill) => self.value(fill)?,
            None => 0,
        };
        if fill > 255 {
            return Err("Fill out of range".into());
        }
        if count > ROM_LIMIT {
            return Err("Allocation too large".into());
        }
        if self.section.is_ram() {
            let limit = if self.section == Section::ZeroPage {
                ZP_LIMIT
            } else {
                BSS_LIMIT
            };
            if self.position.offset + count > limit {
                return Err("RAM allocation out of range".into());
            }
            self.position.offset += count;
            if self.section == Section::Bss {
                self.max_bss = self.max_bss.max(self.position.offset);
            } else {
                self.max_zp = self.max_zp.max(self.position.offset);
            }
            self.save();
            Ok(Vec::new())
        } else {
            if self.position.offset + count > BANK_SIZE {
                return Err("DS out of range".into());
            }
            let bytes = vec![fill as u8; count];
            self.emit(&bytes)?;
            Ok(bytes)
        }
    }

    /// INCBIN file[, offset[, size]]. The layout pass only needs the size, so
    /// the file is read in the emit pass. Returns the bytes and their count.
    fn incbin(&mut self, operand: &str) -> AsmResult<(Vec<u8>, usize)> {
        let args = source::arguments(operand)?;
        if args.is_empty() || args.len() > 3 {
            return Err("INCBIN expects file[, offset[, size]]".into());
        }
        let name = source::quoted(&args[0])?;
        let path = self.files.find(self.request, Path::new(&name))?;
        self.depend(&path);
        let length = self.files.len(&path)?;
        let offset = match args.get(1) {
            Some(offset) => u64::from(self.value(offset)?),
            None => 0,
        };
        let size = match args.get(2) {
            Some(size) => u64::from(self.value(size)?),
            None => length
                .checked_sub(offset)
                .ok_or("INCBIN offset out of range")?,
        };
        let end = offset.checked_add(size).ok_or("INCBIN range overflow")?;
        if end > length {
            return Err("INCBIN range out of bounds".into());
        }
        let available = if self.procs.inside() {
            BANK_SIZE.checked_sub(self.position.offset)
        } else {
            ROM_LIMIT.checked_sub(self.position.linear())
        };
        if available.is_none_or(|a| size > a as u64) {
            return Err(AsmError::fatal(if self.procs.inside() {
                "Procedure exceeds 8 KiB"
            } else {
                "ROM limit exceeded"
            }));
        }
        let size = size as usize;
        let bytes = if self.pass.is_emitting() {
            self.files.read_range(&path, offset, size)?
        } else {
            Vec::new()
        };
        let page = if self.procs.inside() {
            self.position.page
        } else {
            (self.position.page + (self.position.offset + size) / BANK_SIZE) & 7
        };
        if self.pass.is_emitting() {
            self.emit_buffer(&bytes)?;
        } else {
            self.advance_buffer(size)?;
        }
        self.position.page = page;
        Ok((bytes, size))
    }

    /// INCCHR "image.pcx"[, x, y, width, height]: tiles are converted once.
    fn incchr(&mut self, operand: &str) -> AsmResult<Rc<[u8]>> {
        let args = source::arguments(operand)?;
        if args.is_empty() {
            return Err("INCCHR expects a PCX file".into());
        }
        let name = source::quoted(&args[0])?;
        let path = self.files.find(self.request, Path::new(&name))?;
        self.depend(&path);
        let nums = args[1..]
            .iter()
            .map(|a| self.value(a).map(|v| v as usize))
            .collect::<AsmResult<Vec<_>>>()?;
        let key = (path, nums);
        if let Some(tiles) = self.cache.tiles.get(&key) {
            return Ok(Rc::clone(tiles));
        }
        let data = self.files.read(&key.0, PCX_LIMIT)?;
        if data.len() as u64 > PCX_LIMIT {
            return Err("PCX file is too large".into());
        }
        let tiles: Rc<[u8]> = image::pcx_tiles(&data, &key.1)?.into();
        self.cache.tiles.insert(key, Rc::clone(&tiles));
        Ok(tiles)
    }

    fn ines(&mut self, directive: Directive, operand: &str) -> AsmResult<()> {
        let n = self.value(operand)?;
        let limit = match directive {
            Directive::Inesprg | Directive::Ineschr => 64,
            Directive::Inesmap => 255,
            _ => 15,
        };
        if n > limit {
            return Err("iNES parameter out of range".into());
        }
        let n = n as u8;
        match directive {
            Directive::Inesprg => self.header[4] = n,
            Directive::Ineschr => self.header[5] = n,
            Directive::Inesmap => {
                self.header[6] = (self.header[6] & 0x0f) | ((n & 0x0f) << 4);
                self.header[7] = n & 0xf0;
            }
            _ => self.header[6] = (self.header[6] & 0xf0) | n,
        }
        Ok(())
    }

    fn opt(&mut self, operand: &str) -> AsmResult<()> {
        for opt in source::arguments(operand)? {
            let opt = opt.to_ascii_lowercase();
            let flag = opt.ends_with('+');
            if !flag && !opt.ends_with('-') {
                return Err("Invalid OPT flag".into());
            }
            match &opt[..opt.len() - 1] {
                // As in C#, only .LIST requests a listing file; OPT l+ just
                // toggles listing, and OPT m overrides the -m default.
                "l" => self.listing.enabled = flag,
                "m" => self.listing.macros = flag,
                "w" => self.warn = flag,
                "o" => {}
                _ => return Err("Unknown OPT".into()),
            }
        }
        Ok(())
    }
}

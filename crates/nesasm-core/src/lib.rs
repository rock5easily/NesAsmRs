mod engine;
mod error;
mod expr;
mod image;
mod opcode;
mod output;
mod source;
mod state;

pub use engine::{assemble, assemble_with_cancel};
pub use output::{Artifact, ArtifactKind, write_artifacts};
pub use source::resolve_path;

#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SourceEncoding {
    #[default]
    Utf8,
    Sjis,
}

/// Listing detail, serialized as the legacy number 0-3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum ListLevel {
    /// No listing, even after `.LIST`.
    Off = 0,
    /// Source lines with addresses only.
    Brief = 1,
    /// Data bytes of DB/DW lines as well (the default).
    #[default]
    Normal = 2,
    /// Every byte, including DEFCHR tiles.
    Full = 3,
}

impl ListLevel {
    /// Clamps any number to the nearest level, as the CLI `-l` option does.
    pub fn clamped(level: i64) -> Self {
        match level {
            i64::MIN..=0 => Self::Off,
            1 => Self::Brief,
            2 => Self::Normal,
            _ => Self::Full,
        }
    }
}

impl TryFrom<u8> for ListLevel {
    type Error = String;
    fn try_from(level: u8) -> Result<Self, String> {
        match level {
            0 => Ok(Self::Off),
            1 => Ok(Self::Brief),
            2 => Ok(Self::Normal),
            3 => Ok(Self::Full),
            _ => Err("Listing level must be 0 through 3".into()),
        }
    }
}

impl Serialize for ListLevel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(*self as u8)
    }
}

impl<'de> Deserialize<'de> for ListLevel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(u8::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "schema")]
impl JsonSchema for ListLevel {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ListLevel".into()
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "integer", "minimum": 0, "maximum": 3 })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct AssembleOptions {
    /// Source text encoding (default utf8).
    pub encoding: SourceEncoding,
    /// Omit the 16-byte iNES header from the ROM.
    pub raw: bool,
    /// Select zero-page addressing automatically for values below $100.
    pub auto_zp: bool,
    /// Listing detail 0-3 (default 2); a listing is produced only after `.LIST`.
    pub list_level: ListLevel,
    /// List expanded macro bodies.
    pub macro_listing: bool,
    /// Produce a Motorola S-record (.s28) instead of a .nes ROM.
    pub srec: bool,
    /// Suppress warnings.
    pub warning_disabled: bool,
}
impl Default for AssembleOptions {
    fn default() -> Self {
        Self {
            encoding: SourceEncoding::Utf8,
            raw: false,
            auto_zp: false,
            list_level: ListLevel::Normal,
            macro_listing: false,
            srec: false,
            warning_disabled: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AssembleRequest {
    pub input: PathBuf,
    pub working_directory: PathBuf,
    pub include_paths: Vec<PathBuf>,
    /// When present, every dependency must resolve within this canonical root.
    pub allowed_root: Option<PathBuf>,
    pub options: AssembleOptions,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct SourceLocation {
    pub file: PathBuf,
    pub line: usize,
    pub column: Option<usize>,
}

impl SourceLocation {
    /// A location for a whole file (line 0), such as an output path.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            file: path.into(),
            line: 0,
            column: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
        })
    }
}

/// Stable diagnostic codes; serialized as their legacy strings (`E_IO`, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum DiagnosticCode {
    #[serde(rename = "E_IO")]
    Io,
    #[serde(rename = "E_ASSEMBLY")]
    Assembly,
    #[serde(rename = "E_SYMBOL")]
    Symbol,
    #[serde(rename = "E_MACRO")]
    Macro,
    #[serde(rename = "E_CONDITIONAL")]
    Conditional,
    #[serde(rename = "E_INCLUDE")]
    Include,
    #[serde(rename = "E_PROC")]
    Procedure,
    #[serde(rename = "E_LIMIT")]
    Limit,
    #[serde(rename = "E_CANCELLED")]
    Cancelled,
    #[serde(rename = "E_OUTPUT")]
    Output,
    #[serde(rename = "E_TIMEOUT")]
    Timeout,
    #[serde(rename = "E_INTERNAL")]
    Internal,
    #[serde(rename = "W_BANK_OVERFLOW")]
    BankOverflow,
}

impl DiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "E_IO",
            Self::Assembly => "E_ASSEMBLY",
            Self::Symbol => "E_SYMBOL",
            Self::Macro => "E_MACRO",
            Self::Conditional => "E_CONDITIONAL",
            Self::Include => "E_INCLUDE",
            Self::Procedure => "E_PROC",
            Self::Limit => "E_LIMIT",
            Self::Cancelled => "E_CANCELLED",
            Self::Output => "E_OUTPUT",
            Self::Timeout => "E_TIMEOUT",
            Self::Internal => "E_INTERNAL",
            Self::BankOverflow => "W_BANK_OVERFLOW",
        }
    }
}

impl std::fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl PartialEq<&str> for DiagnosticCode {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum DataType {
    #[serde(rename = "DB")]
    Bytes,
    #[serde(rename = "INCBIN")]
    Binary,
    #[serde(rename = "INCCHR")]
    Characters,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagnosticCode,
    pub message: String,
    pub location: SourceLocation,
    pub expansion_trace: Vec<SourceLocation>,
}

impl Diagnostic {
    pub fn error(
        code: DiagnosticCode,
        message: impl Into<String>,
        location: SourceLocation,
    ) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message: message.into(),
            location,
            expansion_trace: Vec::new(),
        }
    }
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Bank of a symbol. Serialized as the legacy number: the ROM bank, `0xF0` for
/// constants and RAM labels, or `0xF1` for a procedure not yet relocated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BankRef {
    Rom(u8),
    Constant,
    Procedure,
}

impl BankRef {
    const CONSTANT: u16 = 0xf0;
    const PROCEDURE: u16 = 0xf1;

    /// The legacy numeric value, as `BANK()` returns it.
    pub const fn number(self) -> u16 {
        match self {
            Self::Rom(bank) => bank as u16,
            Self::Constant => Self::CONSTANT,
            Self::Procedure => Self::PROCEDURE,
        }
    }
    pub const fn rom(self) -> Option<u8> {
        match self {
            Self::Rom(bank) => Some(bank),
            _ => None,
        }
    }
}

impl Serialize for BankRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u16(self.number())
    }
}

impl<'de> Deserialize<'de> for BankRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u16::deserialize(deserializer)? {
            Self::CONSTANT => Ok(Self::Constant),
            Self::PROCEDURE => Ok(Self::Procedure),
            n => u8::try_from(n)
                .ok()
                .filter(|n| usize::from(*n) < state::MAX_BANKS)
                .map(Self::Rom)
                .ok_or_else(|| serde::de::Error::custom("invalid bank")),
        }
    }
}

#[cfg(feature = "schema")]
impl JsonSchema for BankRef {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "BankRef".into()
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "integer",
            "description": "ROM bank 0-127, 240 for constants/RAM, 241 for unrelocated procedures"
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Symbol {
    pub name: String,
    pub value: u32,
    pub bank: BankRef,
    /// CPU page of an address label; `None` for constants (C# page -1).
    pub page: Option<usize>,
    pub location: SourceLocation,
    pub public: bool,
    pub size: usize,
    pub data_type: Option<DataType>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Region {
    pub name: String,
    pub begin: Option<usize>,
    pub end: Option<usize>,
    pub size: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct BankUsage {
    pub bank: usize,
    /// Name given by `.BANK bank, "name"`.
    #[serde(default)]
    pub name: Option<String>,
    pub used: usize,
    pub capacity: usize,
    /// Contiguous runs of one section, in bank order.
    #[serde(default)]
    pub segments: Vec<Segment>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    ZeroPage,
    Bss,
    Code,
    Data,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Segment {
    pub section: SectionKind,
    /// CPU address of the first byte.
    pub start: usize,
    pub size: usize,
}

/// Highest RAM addresses used (exclusive), as offsets from the RAM base.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct RamUsage {
    pub zero_page_end: usize,
    pub bss_end: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AssembleResult {
    /// True when no diagnostic is an error.
    pub success: bool,
    /// ROM payload, without the iNES header (matches C# ResultBinary).
    pub binary: Vec<u8>,
    pub map: Vec<u8>,
    pub header: Vec<u8>,
    pub diagnostics: Vec<Diagnostic>,
    pub symbols: BTreeMap<String, Symbol>,
    pub regions: BTreeMap<String, Region>,
    pub banks: Vec<BankUsage>,
    #[serde(default)]
    pub ram: RamUsage,
    pub dependencies: Vec<PathBuf>,
    pub listing: Option<String>,
    pub srec: Option<String>,
}

impl AssembleResult {
    pub fn error_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.is_error()).count()
    }
    /// Records an error and marks the result as failed.
    pub fn push_error(&mut self, diagnostic: Diagnostic) {
        self.success = false;
        self.diagnostics.push(diagnostic);
    }
}

/// Assembles `request` and, on success, writes its artifacts next to the input
/// (or to `output`), within `request.allowed_root` when set. Output errors are
/// reported as `E_OUTPUT` diagnostics.
pub fn build(
    request: &AssembleRequest,
    output: Option<&Path>,
    cancel: &AtomicBool,
) -> (AssembleResult, Vec<Artifact>) {
    let mut result = assemble_with_cancel(request, cancel);
    if !result.success {
        return (result, Vec::new());
    }
    match write_artifacts(
        &result,
        &request.input,
        output,
        &request.working_directory,
        request.allowed_root.as_deref(),
        &request.options,
    ) {
        Ok(artifacts) => (result, artifacts),
        Err(message) => {
            let file = output.map_or_else(|| request.input.with_extension("nes"), Path::to_owned);
            result.push_error(Diagnostic::error(
                DiagnosticCode::Output,
                message,
                SourceLocation::file(file),
            ));
            (result, Vec::new())
        }
    }
}

/// Documentation topics served by [`reference`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ReferenceTopic {
    #[default]
    Index,
    Instructions,
    Directives,
    Expressions,
    Options,
}

impl ReferenceTopic {
    pub const ALL: [Self; 5] = [
        Self::Index,
        Self::Instructions,
        Self::Directives,
        Self::Expressions,
        Self::Options,
    ];
    /// A one-line summary of the topic.
    pub const fn summary(self) -> &'static str {
        match self {
            Self::Index => "Overview of the NESASM syntax and a minimal program",
            Self::Instructions => "6502 instructions, addressing modes and operand extensions",
            Self::Directives => "Assembler directives and assembler limits",
            Self::Expressions => "Numbers, operators, functions and labels in expressions",
            Self::Options => "Command-line options, include paths and exit status",
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Instructions => "instructions",
            Self::Directives => "directives",
            Self::Expressions => "expressions",
            Self::Options => "options",
        }
    }
    pub const fn text(self) -> &'static str {
        match self {
            Self::Index => include_str!("../../../docs/reference/index.md"),
            Self::Instructions => include_str!("../../../docs/reference/instructions.md"),
            Self::Directives => include_str!("../../../docs/reference/directives.md"),
            Self::Expressions => include_str!("../../../docs/reference/expressions.md"),
            Self::Options => include_str!("../../../docs/reference/options.md"),
        }
    }
}

impl std::str::FromStr for ReferenceTopic {
    type Err = String;
    fn from_str(topic: &str) -> Result<Self, String> {
        match topic.to_ascii_lowercase().as_str() {
            "index" => Ok(Self::Index),
            "instructions" => Ok(Self::Instructions),
            "directives" => Ok(Self::Directives),
            "expressions" => Ok(Self::Expressions),
            "options" => Ok(Self::Options),
            _ => Err(
                "Unknown topic; use index, instructions, directives, expressions, or options"
                    .into(),
            ),
        }
    }
}

/// Reference text for a topic name (`index` when `None`).
pub fn reference(topic: Option<&str>) -> Result<&'static str, String> {
    Ok(topic
        .map_or(Ok(ReferenceTopic::Index), str::parse::<ReferenceTopic>)?
        .text())
}

mod engine;
mod expr;
mod image;
mod opcode;
mod output;
mod source;
mod state;

pub use engine::{assemble, assemble_with_cancel};
pub use output::{Artifact, ArtifactKind, write_artifacts};
pub use source::resolve_path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceEncoding {
    #[default]
    Utf8,
    Sjis,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AssembleOptions {
    pub encoding: SourceEncoding,
    pub raw: bool,
    pub auto_zp: bool,
    pub list_level: u8,
    pub macro_listing: bool,
    pub srec: bool,
    pub warning_disabled: bool,
}
impl Default for AssembleOptions {
    fn default() -> Self {
        Self {
            encoding: SourceEncoding::Utf8,
            raw: false,
            auto_zp: false,
            list_level: 2,
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SourceLocation {
    pub file: PathBuf,
    pub line: usize,
    pub column: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DataType {
    #[serde(rename = "DB")]
    Bytes,
    #[serde(rename = "INCBIN")]
    Binary,
    #[serde(rename = "INCCHR")]
    Characters,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub location: SourceLocation,
    pub expansion_trace: Vec<SourceLocation>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Symbol {
    pub name: String,
    pub value: u32,
    pub bank: usize,
    pub page: usize,
    pub location: SourceLocation,
    pub public: bool,
    pub size: usize,
    pub data_type: Option<DataType>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Region {
    pub name: String,
    pub begin: Option<usize>,
    pub end: Option<usize>,
    pub size: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BankUsage {
    pub bank: usize,
    pub used: usize,
    pub capacity: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AssembleResult {
    pub success: bool,
    /// ROM payload, without the iNES header (matches C# ResultBinary).
    pub binary: Vec<u8>,
    pub map: Vec<u8>,
    pub header: Vec<u8>,
    pub diagnostics: Vec<Diagnostic>,
    pub symbols: BTreeMap<String, Symbol>,
    pub regions: BTreeMap<String, Region>,
    pub banks: Vec<BankUsage>,
    pub dependencies: Vec<PathBuf>,
    pub listing: Option<String>,
    pub srec: Option<String>,
}

pub fn reference(topic: Option<&str>) -> Result<&'static str, String> {
    match topic.unwrap_or("index").to_ascii_lowercase().as_str() {
        "index" => Ok(include_str!("../../../docs/reference/index.md")),
        "instructions" => Ok(include_str!("../../../docs/reference/instructions.md")),
        "directives" => Ok(include_str!("../../../docs/reference/directives.md")),
        "expressions" => Ok(include_str!("../../../docs/reference/expressions.md")),
        "options" => Ok(include_str!("../../../docs/reference/options.md")),
        _ => Err(
            "Unknown topic; use index, instructions, directives, expressions, or options".into(),
        ),
    }
}

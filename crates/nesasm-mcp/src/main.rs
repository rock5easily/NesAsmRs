use nesasm_core::{
    Artifact, AssembleOptions, AssembleRequest, AssembleResult, BankUsage, Diagnostic,
    DiagnosticCode, LineRecord, ReferenceTopic, Region, Severity, SourceLocation, Symbol,
    disasm::{
        self, Address, Diff, DisasmConfig, Disassembly, Issue, IssueKind, RamUse, Routine,
        SourceLine, Summary, Verification, Xref,
    },
    inspect::{Disassembled, HexRow, InesHeader, Rom, Vectors},
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, ListResourcesResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
        ResourceContents, ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
/// Reference documents are resources at `nesasm://reference/<topic>`.
const REFERENCE_URI: &str = "nesasm://reference/";
const DEFAULT_LINE_LIMIT: usize = 200;
const MAX_LINE_LIMIT: usize = 2000;
const DEFAULT_DUMP_LENGTH: usize = 256;
const MAX_DUMP_LENGTH: usize = 4096;
const DEFAULT_DISASSEMBLY_COUNT: usize = 32;
const MAX_DISASSEMBLY_COUNT: usize = 512;
/// Largest ROM file inspect_rom and the disassembler read.
const MAX_ROM_FILE: u64 = 8 * 1024 * 1024;
/// Largest annotation file the disassembler reads.
const MAX_ANNOTATIONS_FILE: u64 = 8 * 1024 * 1024;
const DEFAULT_ISSUE_LIMIT: usize = 20;
const DEFAULT_DIFF_LIMIT: usize = 20;
const DEFAULT_ITEM_LIMIT: usize = 100;
const MAX_ITEM_LIMIT: usize = 1000;
/// Bytes a view without `to` covers.
const DEFAULT_VIEW_SPAN: u32 = 0x100;

/// The project to assemble: an entry file plus optional in-memory sources.
#[derive(Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Project {
    /// Entry .asm file, relative to the server project root.
    input: PathBuf,
    /// In-memory source files by path relative to the root (for example
    /// {"main.asm": "...", "lib/macros.inc": "..."}). They take precedence over
    /// files on disk and are never written; INCLUDE finds them like files.
    #[serde(default)]
    sources: BTreeMap<PathBuf, String>,
    /// Extra directories searched by INCLUDE/INCBIN/INCCHR; must be inside the root.
    #[serde(default)]
    include_paths: Vec<PathBuf>,
    #[serde(default)]
    options: AssembleOptions,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AssemblyInput {
    #[serde(flatten)]
    project: Project,
    /// Return the assembled lines (source line, bank, CPU address, bytes),
    /// filtered and paginated, to check what each line produced.
    lines: Option<LineQuery>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BuildInput {
    #[serde(flatten)]
    project: Project,
    /// Return the assembled lines, as for check.
    lines: Option<LineQuery>,
    /// ROM output path within the project root, ending in .nes or .bin and outside
    /// hidden directories; defaults to input stem + .nes. Listing and S-record
    /// files use the same stem.
    output: Option<PathBuf>,
}

/// Which assembled lines to return. All filters are optional and combined.
#[derive(Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LineQuery {
    /// Only lines of this source file (relative to the root).
    file: Option<PathBuf>,
    /// First source line number (inclusive).
    from_line: Option<usize>,
    /// Last source line number (inclusive).
    to_line: Option<usize>,
    /// Only lines placed in this ROM bank.
    bank: Option<u8>,
    /// First CPU address (inclusive), as a number (49152 for $C000).
    from_address: Option<u32>,
    /// Last CPU address (inclusive).
    to_address: Option<u32>,
    /// Matching lines to skip, for paging.
    #[serde(default)]
    offset: usize,
    /// Maximum lines returned (default 200, at most 2000).
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReferenceInput {
    /// Documentation topic (default index).
    topic: Option<ReferenceTopic>,
}

/// A bank and CPU address range of a ROM.
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RomRange {
    /// ROM bank (8 KiB units, as in .BANK).
    bank: usize,
    /// CPU address in that bank, as a number (49152 for $C000).
    address: u32,
    /// Bytes for hexdump (default 256, at most 4096) or instructions for
    /// disassemble (default 32, at most 512).
    length: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InspectInput {
    /// A ROM file within the root: .nes with an iNES header, or a raw .bin.
    rom: Option<PathBuf>,
    /// Or a project to assemble in memory (nothing is written); its symbols
    /// name labels and operands in the disassembly.
    project: Option<Project>,
    /// Hex dump of a range.
    hexdump: Option<RomRange>,
    /// Disassembly starting at an address.
    disassemble: Option<RomRange>,
}

/// Which issues to return.
#[derive(Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IssueQuery {
    /// Only issues of this kind.
    kind: Option<IssueKind>,
    #[serde(default)]
    offset: usize,
    /// Maximum issues returned (default 20, at most 1000).
    limit: Option<usize>,
}

/// Generated source lines of an address range.
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViewQuery {
    /// Only this bank (needed when several banks hold the range).
    bank: Option<usize>,
    /// First CPU address: a number or "$C000".
    from: Address,
    /// Last CPU address (default from + $FF).
    to: Option<Address>,
    #[serde(default)]
    offset: usize,
    /// Maximum lines returned (default 200, at most 2000).
    limit: Option<usize>,
}

#[derive(Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    #[serde(default)]
    offset: usize,
    /// Maximum items returned (default 100, at most 1000).
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DisasmInput {
    /// Annotation file (JSON) within the root. Its `rom` field names the ROM;
    /// the other fields record what is known (labels, symbols, regions,
    /// comments). Edit it and call again to refine the source.
    annotations: Option<PathBuf>,
    /// Or the annotations inline, with the same fields.
    inline: Option<DisasmConfig>,
    /// Main source file (.asm) within the root, used for the names of bank
    /// files and the CHR file; defaults to the ROM path with .asm.
    output: Option<PathBuf>,
    /// Issues to return (default: the first 20).
    issues: Option<IssueQuery>,
    /// For a ROM hack (annotations with `base`): the changed ranges written
    /// as source, with their code/data bytes, labels, routines and references
    /// from outside (default: the first 20).
    diffs: Option<PageQuery>,
    /// Generated source lines (with their bytes) in an address range.
    view: Option<ViewQuery>,
    /// References to each label, symbol name or address ("$C000").
    xrefs: Option<Vec<String>>,
    /// Routines with their callers, calls and RAM accesses.
    routines: Option<PageQuery>,
    /// Addresses outside the ROM (RAM, I/O) and the routines accessing them.
    ram: Option<PageQuery>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExportInput {
    /// Annotation file (JSON) within the root, as for disassemble.
    annotations: Option<PathBuf>,
    /// Or the annotations inline.
    inline: Option<DisasmConfig>,
    /// Main source file to write (.asm) within the root and outside hidden
    /// directories; defaults to the ROM path with .asm. Bank files
    /// (<stem>_bankXX.asm, XX the bank in uppercase hexadecimal) and the CHR
    /// file (<stem>.chr) use its stem.
    output: Option<PathBuf>,
}

#[derive(Serialize, JsonSchema)]
struct Page<T> {
    /// Items matching the query.
    total: usize,
    offset: usize,
    items: Vec<T>,
}

impl<T: Clone> Page<T> {
    fn of(items: &[T], offset: usize, limit: Option<usize>, default: usize, max: usize) -> Self {
        Self {
            total: items.len(),
            offset,
            items: items
                .iter()
                .skip(offset)
                .take(limit.unwrap_or(default).min(max))
                .cloned()
                .collect(),
        }
    }
}

#[derive(Default, Serialize, JsonSchema)]
struct DisasmReport {
    success: bool,
    /// Errors in the annotations or ROM, or why the request failed.
    diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<Summary>,
    /// Whether the generated source reassembles to the same ROM.
    #[serde(skip_serializing_if = "Option::is_none")]
    verification: Option<Verification>,
    /// Generated source files relative to the root (main file first).
    files: Vec<PathBuf>,
    /// Files written by export_disassembly.
    artifacts: Vec<Artifact>,
    /// Unedited bank files of the replaced source that the new source no
    /// longer includes (older decimal names such as <stem>_bank10.asm for
    /// bank 10, or banks a ROM hack no longer changes), removed by
    /// export_disassembly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    removed: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issues: Option<Page<Issue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diffs: Option<Page<Diff>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    view: Option<Page<SourceLine>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    xrefs: Option<BTreeMap<String, Vec<Xref>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    routines: Option<Page<Routine>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ram: Option<Page<RamUse>>,
}

#[derive(Default, Serialize, JsonSchema)]
struct Report {
    success: bool,
    diagnostics: Vec<Diagnostic>,
    symbols: BTreeMap<String, Symbol>,
    regions: BTreeMap<String, Region>,
    banks: Vec<BankUsage>,
    dependencies: Vec<PathBuf>,
    artifacts: Vec<Artifact>,
    /// Assembled lines, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    lines: Option<LinePage>,
}

#[derive(Serialize, JsonSchema)]
struct LinePage {
    /// Lines matching the query.
    total: usize,
    offset: usize,
    lines: Vec<LineView>,
}

#[derive(Serialize, JsonSchema)]
struct LineView {
    /// Source file relative to the root.
    file: PathBuf,
    line: usize,
    /// "file:line" of the macro call, for lines expanded from a macro.
    #[serde(skip_serializing_if = "Option::is_none")]
    called_from: Option<String>,
    /// ROM bank; absent in the ZP and BSS sections.
    #[serde(skip_serializing_if = "Option::is_none")]
    bank: Option<u8>,
    /// CPU address before the line.
    address: u32,
    /// Bytes produced as hex pairs (the first 64 for large data).
    bytes: String,
    /// Number of bytes produced.
    size: usize,
    text: String,
}

#[derive(Serialize, JsonSchema)]
struct ReferenceReport {
    success: bool,
    topic: String,
    text: String,
}

#[derive(Default, Serialize, JsonSchema)]
struct InspectReport {
    success: bool,
    /// Assembly diagnostics, or why the request failed.
    diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    header: Option<InesHeader>,
    /// Number of 8 KiB banks of PRG and CHR data.
    banks: usize,
    /// NMI/RESET/IRQ vectors of the banks that hold $FFFA-$FFFF.
    vectors: Vec<Vectors>,
    /// Problems that would stop the ROM from loading or starting.
    warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hexdump: Option<Vec<HexRow>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    disassembly: Option<Vec<Disassembled>>,
}

#[derive(Clone)]
struct Server {
    root: PathBuf,
    timeout: Duration,
    gate: Arc<tokio::sync::Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

fn schema<T: JsonSchema>() -> Arc<serde_json::Map<String, serde_json::Value>> {
    Arc::new(
        serde_json::to_value(schemars::schema_for!(T))
            .unwrap()
            .as_object()
            .unwrap()
            .clone(),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn error(code: DiagnosticCode, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(code, message, SourceLocation::default())
}

/// Reads a file within `root`, up to `limit` bytes.
fn read_limited(root: &Path, path: &Path, limit: u64) -> Result<(PathBuf, Vec<u8>), Diagnostic> {
    let io = |e: String| error(DiagnosticCode::Io, format!("{}: {e}", path.display()));
    let path = nesasm_core::resolve_path(path, root, Some(root)).map_err(io)?;
    let size = std::fs::metadata(&path)
        .map_err(|e| io(e.to_string()))?
        .len();
    if size > limit {
        return Err(io(format!("File exceeds {} MiB", limit >> 20)));
    }
    let bytes = std::fs::read(&path).map_err(|e| io(e.to_string()))?;
    Ok((path, bytes))
}

/// A structured tool result with a text summary.
fn tool_result<T: Serialize>(report: &T, success: bool, summary: String) -> CallToolResult {
    let mut result = CallToolResult::structured(serde_json::to_value(report).unwrap());
    result.is_error = Some(!success);
    result.content = vec![ContentBlock::text(summary)];
    result
}

fn diagnostics_summary(diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|d| {
            if d.location.file.as_os_str().is_empty() {
                format!("{}: {}", d.severity, d.message)
            } else {
                format!(
                    "{}:{}: {}",
                    d.location.file.display(),
                    d.location.line,
                    d.message
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Server {
    fn relative(root: &Path, path: &Path) -> PathBuf {
        nesasm_core::relative_to_root(path, root).unwrap_or_else(|| path.to_owned())
    }

    /// The core request for a project; in-memory sources are encoded in the
    /// project's source encoding.
    fn request(
        root: &Path,
        project: Project,
        collect_lines: bool,
    ) -> Result<AssembleRequest, String> {
        let files = project
            .sources
            .into_iter()
            .map(|(path, text)| {
                nesasm_core::encode_source(&text, &project.options.encoding)
                    .map(|bytes| (path.clone(), bytes))
                    .map_err(|e| format!("{}: {e}", path.display()))
            })
            .collect::<Result<_, _>>()?;
        Ok(AssembleRequest {
            input: project.input,
            working_directory: root.to_owned(),
            include_paths: project.include_paths,
            allowed_root: Some(root.to_owned()),
            options: project.options,
            files,
            collect_lines,
        })
    }

    /// The page of `lines` selected by `query`, with root-relative paths.
    fn line_page(root: &Path, lines: &[LineRecord], query: &LineQuery) -> LinePage {
        let file = query.file.as_deref();
        let matching: Vec<&LineRecord> = lines
            .iter()
            .filter(|r| file.is_none_or(|f| Self::relative(root, &r.location.file) == f))
            .filter(|r| query.from_line.is_none_or(|l| r.location.line >= l))
            .filter(|r| query.to_line.is_none_or(|l| r.location.line <= l))
            .filter(|r| query.bank.is_none_or(|b| r.bank == Some(b)))
            .filter(|r| query.from_address.is_none_or(|a| r.address >= a))
            .filter(|r| query.to_address.is_none_or(|a| r.address <= a))
            .collect();
        let limit = query
            .limit
            .unwrap_or(DEFAULT_LINE_LIMIT)
            .min(MAX_LINE_LIMIT);
        LinePage {
            total: matching.len(),
            offset: query.offset,
            lines: matching
                .into_iter()
                .skip(query.offset)
                .take(limit)
                .map(|r| LineView {
                    file: Self::relative(root, &r.location.file),
                    line: r.location.line,
                    called_from: r
                        .called_from
                        .as_ref()
                        .map(|c| format!("{}:{}", Self::relative(root, &c.file).display(), c.line)),
                    bank: r.bank,
                    address: r.address,
                    bytes: hex(&r.bytes),
                    size: r.size,
                    text: r.text.clone(),
                })
                .collect(),
        }
    }

    /// Runs blocking work under the server-wide gate and time limit. On
    /// timeout the cancel flag is set; the engine stops at its next line.
    async fn guarded<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Path, &AtomicBool) -> T + Send + 'static,
    ) -> Result<T, Diagnostic> {
        let guard = self.gate.clone().lock_owned().await;
        let root = self.root.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let mut task = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            work(&root, &worker_cancel)
        });
        let (joined, timed_out) = match tokio::time::timeout(self.timeout, &mut task).await {
            Ok(joined) => (joined, false),
            Err(_) => {
                cancel.store(true, Ordering::Relaxed);
                (task.await, true)
            }
        };
        if timed_out {
            return Err(error(
                DiagnosticCode::Timeout,
                format!(
                    "Assembly exceeded the {} second time limit",
                    self.timeout.as_secs()
                ),
            ));
        }
        joined.map_err(|e| error(DiagnosticCode::Internal, e.to_string()))
    }

    async fn execute(
        &self,
        project: Project,
        lines: Option<LineQuery>,
        write: bool,
        output: Option<PathBuf>,
    ) -> CallToolResult {
        let report = self
            .guarded(move |root, cancel| {
                let request = match Self::request(root, project, lines.is_some()) {
                    Ok(request) => request,
                    Err(e) => {
                        return Report {
                            diagnostics: vec![error(DiagnosticCode::Io, e)],
                            ..Report::default()
                        };
                    }
                };
                let (result, artifacts) = if write {
                    nesasm_core::build(&request, output.as_deref(), cancel)
                } else {
                    (
                        nesasm_core::assemble_with_cancel(&request, cancel),
                        Vec::new(),
                    )
                };
                let page = lines.map(|query| Self::line_page(root, &result.lines, &query));
                Report {
                    success: result.success,
                    diagnostics: result.diagnostics,
                    symbols: result.symbols,
                    regions: result.regions,
                    banks: result.banks,
                    dependencies: result.dependencies,
                    artifacts,
                    lines: page,
                }
            })
            .await
            .unwrap_or_else(|diagnostic| Report {
                diagnostics: vec![diagnostic],
                ..Report::default()
            });
        let summary = if report.success {
            format!(
                "Assembly succeeded; {} artifacts, {} symbols",
                report.artifacts.len(),
                report.symbols.len()
            )
        } else {
            diagnostics_summary(&report.diagnostics)
        };
        tool_result(&report, report.success, summary)
    }

    /// Loads the ROM to inspect: a file, or a project assembled in memory.
    fn load_rom(
        root: &Path,
        input: &InspectInput,
        cancel: &AtomicBool,
    ) -> Result<(Rom, AssembleResult), Vec<Diagnostic>> {
        match (&input.rom, &input.project) {
            (Some(path), None) => {
                let path = nesasm_core::resolve_path(path, root, Some(root))
                    .map_err(|e| vec![error(DiagnosticCode::Io, e)])?;
                let size = std::fs::metadata(&path)
                    .map_err(|e| vec![error(DiagnosticCode::Io, e.to_string())])?
                    .len();
                if size > MAX_ROM_FILE {
                    return Err(vec![error(DiagnosticCode::Io, "ROM file exceeds 8 MiB")]);
                }
                let bytes = std::fs::read(&path)
                    .map_err(|e| vec![error(DiagnosticCode::Io, e.to_string())])?;
                let rom = Rom::from_file(&bytes).map_err(|e| vec![error(DiagnosticCode::Io, e)])?;
                Ok((rom, AssembleResult::default()))
            }
            (None, Some(project)) => {
                let request = Self::request(root, project.clone(), false)
                    .map_err(|e| vec![error(DiagnosticCode::Io, e)])?;
                let result = nesasm_core::assemble_with_cancel(&request, cancel);
                if !result.success {
                    return Err(result.diagnostics);
                }
                Ok((Rom::from_assembly(&result), result))
            }
            _ => Err(vec![error(
                DiagnosticCode::Io,
                "Give either rom (a ROM file) or project (sources to assemble)",
            )]),
        }
    }

    fn inspect_report(root: &Path, input: &InspectInput, cancel: &AtomicBool) -> InspectReport {
        let (rom, result) = match Self::load_rom(root, input, cancel) {
            Ok(loaded) => loaded,
            Err(diagnostics) => {
                return InspectReport {
                    diagnostics,
                    ..InspectReport::default()
                };
            }
        };
        let mut report = InspectReport {
            success: true,
            diagnostics: result.diagnostics.clone(),
            header: rom.header.clone(),
            banks: rom.bank_count(),
            vectors: rom.vectors(),
            warnings: rom.warnings(),
            ..InspectReport::default()
        };
        if let Some(range) = &input.hexdump {
            let length = range
                .length
                .unwrap_or(DEFAULT_DUMP_LENGTH)
                .min(MAX_DUMP_LENGTH);
            match rom.hexdump(range.bank, range.address, length) {
                Ok(rows) => report.hexdump = Some(rows),
                Err(e) => report.diagnostics.push(error(DiagnosticCode::Io, e)),
            }
        }
        if let Some(range) = &input.disassemble {
            let count = range
                .length
                .unwrap_or(DEFAULT_DISASSEMBLY_COUNT)
                .min(MAX_DISASSEMBLY_COUNT);
            match rom.disassemble(range.bank, range.address, count, &result.symbols) {
                Ok(lines) => report.disassembly = Some(lines),
                Err(e) => report.diagnostics.push(error(DiagnosticCode::Io, e)),
            }
        }
        report.success = report.diagnostics.iter().all(|d| !d.is_error());
        report
    }

    fn inspect_summary(report: &InspectReport) -> String {
        if !report.success {
            return diagnostics_summary(&report.diagnostics);
        }
        let mut text = Vec::new();
        text.push(match &report.header {
            Some(h) => format!(
                "iNES: {}x16 KiB PRG, {}x8 KiB CHR, mapper {}, {:?} mirroring; {} banks",
                h.prg_16k, h.chr_8k, h.mapper, h.mirroring, report.banks
            ),
            None => format!("No iNES header; {} banks", report.banks),
        });
        for v in &report.vectors {
            text.push(format!(
                "Bank {} vectors: NMI ${:04X}, RESET ${:04X}, IRQ ${:04X}",
                v.bank, v.nmi, v.reset, v.irq
            ));
        }
        text.extend(report.warnings.iter().map(|w| format!("Warning: {w}")));
        for row in report.hexdump.iter().flatten() {
            text.push(format!("{:04X}: {}", row.address, row.bytes));
        }
        for line in report.disassembly.iter().flatten() {
            let mut entry = String::new();
            for label in &line.labels {
                entry.push_str(&format!("{label}:\n"));
            }
            entry.push_str(&format!(
                "{:04X}  {:<9} {}",
                line.address, line.bytes, line.instruction
            ));
            if !line.operand_symbols.is_empty() {
                entry.push_str(&format!("  ; {}", line.operand_symbols.join(", ")));
            }
            if line.cmos_only {
                entry.push_str("  ; 65C02 only");
            }
            text.push(entry);
        }
        text.join("\n")
    }
}

/// A disassembly and the resolved path of its ROM.
struct Loaded {
    disassembly: Disassembly,
    rom: PathBuf,
}

impl Server {
    /// Reads the annotations and ROM and disassembles them.
    fn load_disassembly(
        root: &Path,
        annotations: Option<&Path>,
        inline: Option<&DisasmConfig>,
        output: Option<&Path>,
        cancel: &AtomicBool,
    ) -> Result<Loaded, Vec<Diagnostic>> {
        let (config, location) = match (annotations, inline) {
            (Some(path), None) => {
                let (_, bytes) =
                    read_limited(root, path, MAX_ANNOTATIONS_FILE).map_err(|d| vec![d])?;
                let location = SourceLocation::file(path);
                let config =
                    disasm::parse_config(&String::from_utf8_lossy(&bytes)).map_err(|e| {
                        vec![Diagnostic::error(
                            DiagnosticCode::Disasm,
                            e,
                            location.clone(),
                        )]
                    })?;
                (config, location)
            }
            (None, Some(config)) => (config.clone(), SourceLocation::default()),
            _ => {
                return Err(vec![error(
                    DiagnosticCode::Disasm,
                    "Give either annotations (a JSON file) or inline annotations",
                )]);
            }
        };
        if config.rom.as_os_str().is_empty() {
            return Err(vec![Diagnostic::error(
                DiagnosticCode::Disasm,
                "The annotations must name the ROM file (rom)",
                location,
            )]);
        }
        let (rom, bytes) = read_limited(root, &config.rom, MAX_ROM_FILE).map_err(|d| vec![d])?;
        let base = match &config.base {
            Some(path) => Some(
                read_limited(root, path, MAX_ROM_FILE)
                    .map_err(|d| vec![d])?
                    .1,
            ),
            None => None,
        };
        let output = output.map_or_else(|| disasm::default_output(&config.rom), Path::to_owned);
        if !output
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("asm"))
        {
            return Err(vec![error(
                DiagnosticCode::Output,
                "Source output must have a .asm extension",
            )]);
        }
        // Sources name their bank and CHR files relative to the root.
        let source = nesasm_core::resolve_path(&output, root, Some(root))
            .map_err(|e| vec![error(DiagnosticCode::Output, e)])?;
        let source = Self::relative(root, &source);
        let disassembly = disasm::disassemble(&bytes, base.as_deref(), &config, &source, cancel)
            .map_err(|errors| {
                errors
                    .into_iter()
                    .map(|d| Diagnostic {
                        location: location.clone(),
                        ..d
                    })
                    .collect::<Vec<_>>()
            })?;
        Ok(Loaded { disassembly, rom })
    }

    fn disasm_report(root: &Path, input: &DisasmInput, cancel: &AtomicBool) -> DisasmReport {
        let loaded = match Self::load_disassembly(
            root,
            input.annotations.as_deref(),
            input.inline.as_ref(),
            input.output.as_deref(),
            cancel,
        ) {
            Ok(loaded) => loaded,
            Err(diagnostics) => {
                return DisasmReport {
                    diagnostics,
                    ..DisasmReport::default()
                };
            }
        };
        let d = &loaded.disassembly;
        let mut report = DisasmReport {
            success: true,
            summary: Some(d.summary()),
            verification: Some(d.verify(root, Some(root), cancel)),
            files: d.files().iter().map(|(p, _)| p.clone()).collect(),
            ..DisasmReport::default()
        };
        let query = input.issues.clone().unwrap_or_default();
        let issues: Vec<Issue> = d
            .issues()
            .iter()
            .filter(|i| query.kind.is_none_or(|k| i.kind == k))
            .cloned()
            .collect();
        report.issues = Some(Page::of(
            &issues,
            query.offset,
            query.limit,
            DEFAULT_ISSUE_LIMIT,
            MAX_ITEM_LIMIT,
        ));
        if report.summary.as_ref().is_some_and(|s| s.diff.is_some()) {
            let page = input.diffs.clone().unwrap_or_default();
            report.diffs = Some(Page::of(
                &d.diffs(),
                page.offset,
                page.limit,
                DEFAULT_DIFF_LIMIT,
                MAX_ITEM_LIMIT,
            ));
        }
        if let Some(view) = &input.view {
            let to = view
                .to
                .map_or(view.from.0.saturating_add(DEFAULT_VIEW_SPAN - 1), |a| a.0);
            let lines: Vec<SourceLine> = d
                .view(view.bank, view.from.0, to)
                .into_iter()
                .cloned()
                .collect();
            report.view = Some(Page::of(
                &lines,
                view.offset,
                view.limit,
                DEFAULT_LINE_LIMIT,
                MAX_LINE_LIMIT,
            ));
        }
        if let Some(queries) = &input.xrefs {
            let mut xrefs = BTreeMap::new();
            for query in queries {
                match d.xrefs(query) {
                    Ok(found) => {
                        xrefs.insert(query.clone(), found);
                    }
                    Err(e) => report.diagnostics.push(Diagnostic {
                        severity: Severity::Warning,
                        ..error(DiagnosticCode::Disasm, e)
                    }),
                }
            }
            report.xrefs = Some(xrefs);
        }
        if let Some(page) = &input.routines {
            report.routines = Some(Page::of(
                &d.routines(),
                page.offset,
                page.limit,
                DEFAULT_ITEM_LIMIT,
                MAX_ITEM_LIMIT,
            ));
        }
        if let Some(page) = &input.ram {
            report.ram = Some(Page::of(
                &d.ram(),
                page.offset,
                page.limit,
                DEFAULT_ITEM_LIMIT,
                MAX_ITEM_LIMIT,
            ));
        }
        report
    }

    fn export_report(root: &Path, input: &ExportInput, cancel: &AtomicBool) -> DisasmReport {
        let loaded = match Self::load_disassembly(
            root,
            input.annotations.as_deref(),
            input.inline.as_ref(),
            input.output.as_deref(),
            cancel,
        ) {
            Ok(loaded) => loaded,
            Err(diagnostics) => {
                return DisasmReport {
                    diagnostics,
                    ..DisasmReport::default()
                };
            }
        };
        let d = &loaded.disassembly;
        let verification = d.verify(root, Some(root), cancel);
        let mut report = DisasmReport {
            summary: Some(d.summary()),
            files: d.files().iter().map(|(p, _)| p.clone()).collect(),
            ..DisasmReport::default()
        };
        if !verification.matched {
            report.diagnostics.push(error(
                DiagnosticCode::Disasm,
                "The generated source does not reassemble to the ROM; nothing was written (see verification)",
            ));
        } else if cancel.load(Ordering::Relaxed) {
            // The time limit passed; nothing is written after the reply.
            report.diagnostics.push(error(
                DiagnosticCode::Cancelled,
                "The time limit passed before writing; nothing was written",
            ));
        } else {
            match nesasm_core::export_disassembly(d, root, Some(root), &loaded.rom, cancel) {
                Ok(export) => {
                    report.artifacts = export.artifacts;
                    report.removed = export
                        .removed
                        .iter()
                        .map(|path| Self::relative(root, path))
                        .collect();
                    for warning in export.warnings {
                        report.diagnostics.push(Diagnostic {
                            severity: Severity::Warning,
                            ..error(DiagnosticCode::Output, warning)
                        });
                    }
                    report.success = true;
                }
                Err(e) => report.diagnostics.push(Diagnostic::error(
                    DiagnosticCode::Output,
                    e,
                    SourceLocation::file(d.source_path()),
                )),
            }
        }
        report.verification = Some(verification);
        report
    }

    fn disasm_summary(report: &DisasmReport) -> String {
        let mut text = Vec::new();
        if let Some(summary) = &report.summary {
            let prg = summary.banks.iter().filter(|b| !b.chr).count();
            text.push(format!(
                "{} ROM: {prg} PRG and {} CHR banks; {} instructions, {} labels, {} routines",
                json_name(&summary.format),
                summary.banks.len() - prg,
                summary.instructions,
                summary.labels,
                summary.routines
            ));
            for bank in summary.banks.iter().filter(|b| !b.chr) {
                text.push(format!(
                    "Bank {} {}: code {}, data {}, unknown {} bytes",
                    bank.bank,
                    bank.org.unwrap_or_default(),
                    bank.code_bytes,
                    bank.data_bytes,
                    bank.unknown_bytes
                ));
            }
            if let Some(diff) = &summary.diff {
                let new_banks = if diff.new_banks.is_empty() {
                    String::new()
                } else {
                    let banks: Vec<String> = diff.new_banks.iter().map(usize::to_string).collect();
                    format!("; banks not in the base ROM: {}", banks.join(", "))
                };
                text.push(format!(
                    "ROM hack: {} bytes differ from the base ROM; {} ranges ({} bytes) are source, the rest is included from the base{new_banks}",
                    diff.changed_bytes, diff.ranges, diff.source_bytes
                ));
            }
            if !summary.issues.is_empty() {
                let counts: Vec<String> = summary
                    .issues
                    .iter()
                    .map(|(kind, n)| format!("{} {n}", json_name(kind)))
                    .collect();
                text.push(format!("Issues: {}", counts.join(", ")));
            }
        }
        if let Some(v) = &report.verification {
            text.push(if v.matched {
                "Round trip: the source reassembles to the same ROM".into()
            } else if let Some(first) = v.mismatches.first() {
                format!(
                    "Round trip FAILED: {} bytes differ; first {}{} expected {} actual {}{}",
                    v.mismatch_count,
                    first
                        .bank
                        .map_or("header ".into(), |b| format!("bank {b} ")),
                    first.address,
                    first.expected,
                    first.actual,
                    first
                        .line
                        .as_ref()
                        .map_or(String::new(), |l| format!(" ({l})"))
                )
            } else {
                format!(
                    "Round trip FAILED: the source does not assemble\n{}",
                    diagnostics_summary(&v.diagnostics)
                )
            });
        }
        for artifact in &report.artifacts {
            text.push(format!("Wrote {}", artifact.path.display()));
        }
        for path in &report.removed {
            text.push(format!("Removed {}", path.display()));
        }
        if !report.diagnostics.is_empty() {
            text.push(diagnostics_summary(&report.diagnostics));
        }
        let heading = |name: &str, total: usize, offset: usize, shown: usize| {
            if offset > 0 || offset + shown < total {
                format!("{name}: {shown} of {total} from {offset}")
            } else {
                format!("{name}: {total}")
            }
        };
        if let Some(page) = report.issues.as_ref().filter(|p| p.total > 0) {
            text.push(heading("Issues", page.total, page.offset, page.items.len()));
            for issue in &page.items {
                let bank = issue.bank.map_or(String::new(), |b| format!("bank {b} "));
                text.push(format!(
                    "  {} {bank}{}: {}",
                    json_name(&issue.kind),
                    issue.address,
                    issue.message
                ));
            }
        }
        if let Some(page) = report.diffs.as_ref().filter(|p| p.total > 0) {
            text.push(heading(
                "Changed ranges",
                page.total,
                page.offset,
                page.items.len(),
            ));
            for d in &page.items {
                let mut entry = format!(
                    "  {}bank {} {}-{}: {} changed; code {}, data {}, unknown {}",
                    if d.chr { "CHR " } else { "" },
                    d.bank,
                    d.start,
                    d.end,
                    d.changed_bytes,
                    d.code_bytes,
                    d.data_bytes,
                    d.unknown_bytes
                );
                if let Some(fill) = &d.base_fill {
                    entry.push_str(&format!("; base was free space ({fill})"));
                }
                if !d.labels.is_empty() {
                    entry.push_str(&format!("; labels [{}]", d.labels.join(", ")));
                }
                if !d.routines.is_empty() {
                    entry.push_str(&format!("; in [{}]", d.routines.join(", ")));
                }
                if !d.referenced_from.is_empty() {
                    let from: Vec<String> = d
                        .referenced_from
                        .iter()
                        .map(|x| {
                            let what = if x.line.is_empty() {
                                json_name(&x.kind)
                            } else {
                                x.line.clone()
                            };
                            format!("bank {} {} {what}", x.bank, x.address)
                        })
                        .collect();
                    entry.push_str(&format!("; from [{}]", from.join(", ")));
                }
                text.push(entry);
            }
        }
        if let Some(page) = &report.view {
            text.push(heading("Lines", page.total, page.offset, page.items.len()));
            for line in &page.items {
                let address = line.address.map_or(String::new(), |a| a.to_string());
                // The tab of instructions would break the columns of this table.
                let source = match line.text.strip_prefix('\t') {
                    Some(rest) => format!("  {rest}"),
                    None => line.text.clone(),
                };
                text.push(format!("{address:<6} {:<27} {source}", line.bytes));
            }
        }
        for (query, found) in report.xrefs.iter().flatten() {
            text.push(format!("References to {query}: {}", found.len()));
            for x in found {
                let routine = x
                    .routine
                    .as_ref()
                    .map_or(String::new(), |r| format!(" in {r}"));
                let what = if x.line.is_empty() {
                    json_name(&x.kind)
                } else {
                    x.line.clone()
                };
                text.push(format!("  bank {} {}{routine}: {what}", x.bank, x.address));
            }
        }
        if let Some(page) = &report.routines {
            text.push(heading(
                "Routines",
                page.total,
                page.offset,
                page.items.len(),
            ));
            for r in &page.items {
                text.push(format!(
                    "  {} bank {} {}: {} bytes, {} callers; calls [{}]; reads [{}]; writes [{}]",
                    r.name,
                    r.bank,
                    r.address,
                    r.size,
                    r.callers,
                    r.calls.join(", "),
                    r.reads.join(", "),
                    r.writes.join(", ")
                ));
            }
        }
        if let Some(page) = &report.ram {
            text.push(heading("RAM", page.total, page.offset, page.items.len()));
            for r in &page.items {
                let name = r.name.as_ref().map_or(String::new(), |n| format!(" {n}"));
                text.push(format!(
                    "  {}{name}: {} reads, {} writes; [{}]",
                    r.address,
                    r.reads,
                    r.writes,
                    r.routines.join(", ")
                ));
            }
        }
        text.join("\n")
    }
}

/// The JSON name of a unit enum value (`"nes"`, `"invalid_opcode"`).
fn json_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[tool_router]
impl Server {
    fn new(root: PathBuf, timeout: Duration) -> Self {
        Self {
            root,
            timeout,
            gate: Arc::new(tokio::sync::Mutex::new(())),
            tool_router: Self::tool_router(),
        }
    }
    #[tool(description="Assemble a NESASM project and write ROM, listing and S-record artifacts. Sources can be files in the root or in-memory text (sources). Returns diagnostics, symbols, bank/region usage and, on request, the assembled lines with their addresses and bytes. Paths must stay inside the configured root.",output_schema=schema::<Report>(),annotations(read_only_hint=false,destructive_hint=true))]
    async fn assemble(&self, Parameters(input): Parameters<BuildInput>) -> CallToolResult {
        self.execute(input.project, input.lines, true, input.output)
            .await
    }
    #[tool(description="Validate a NESASM project without writing files. Sources can be files in the root or in-memory text (sources). Returns diagnostics, symbols, dependencies, bank/region usage and, on request, the assembled lines with their addresses and bytes.",output_schema=schema::<Report>(),annotations(read_only_hint=true))]
    async fn check(&self, Parameters(input): Parameters<AssemblyInput>) -> CallToolResult {
        self.execute(input.project, input.lines, false, None).await
    }
    #[tool(description="Inspect a ROM: decode the iNES header, show the NMI/RESET/IRQ vectors and warn about problems that stop it from starting; optionally hex dump or disassemble (NESASM syntax, with symbol names) a bank and address range. The ROM is a file in the root, or a project assembled in memory without writing anything.",output_schema=schema::<InspectReport>(),annotations(read_only_hint=true))]
    async fn inspect_rom(&self, Parameters(input): Parameters<InspectInput>) -> CallToolResult {
        let report = self
            .guarded(move |root, cancel| Self::inspect_report(root, &input, cancel))
            .await
            .unwrap_or_else(|diagnostic| InspectReport {
                diagnostics: vec![diagnostic],
                ..InspectReport::default()
            });
        let summary = Self::inspect_summary(&report);
        tool_result(&report, report.success, summary)
    }
    #[tool(description="Disassemble a ROM (.nes, or raw .bin) into NESASM source, guided by annotations: a JSON file in the root (or inline) that names the ROM and records labels, symbols, code/data regions and comments taken from the author's notes. Code is traced from the interrupt vectors and entry points. For a ROM hack, give the original ROM as base: only the changed ranges become source (widen them with patches) and the rest is included from the base. Returns a summary, issues to settle in the annotations and whether the source reassembles to the same ROM; optionally source lines of a range, references, routines and RAM use. Writes nothing. See get_reference topic disassembler.",output_schema=schema::<DisasmReport>(),annotations(read_only_hint=true))]
    async fn disassemble(&self, Parameters(input): Parameters<DisasmInput>) -> CallToolResult {
        let report = self
            .guarded(move |root, cancel| Self::disasm_report(root, &input, cancel))
            .await
            .unwrap_or_else(|diagnostic| DisasmReport {
                diagnostics: vec![diagnostic],
                ..DisasmReport::default()
            });
        let summary = Self::disasm_summary(&report);
        tool_result(&report, report.success, summary)
    }
    #[tool(description="Write the source that disassemble generates (.asm, plus <stem>_bankXX.asm files with XX the bank in uppercase hexadecimal for large ROMs, and <stem>.chr for CHR ROM), only if it reassembles to the same ROM. Existing files are replaced only if they are generated sources not edited since (a bank file only by the file of the same bank, unless the replaced main source included it); unedited bank files the replaced main source included and the new one does not are removed.",output_schema=schema::<DisasmReport>(),annotations(read_only_hint=false,destructive_hint=true))]
    async fn export_disassembly(
        &self,
        Parameters(input): Parameters<ExportInput>,
    ) -> CallToolResult {
        let report = self
            .guarded(move |root, cancel| Self::export_report(root, &input, cancel))
            .await
            .unwrap_or_else(|diagnostic| DisasmReport {
                diagnostics: vec![diagnostic],
                ..DisasmReport::default()
            });
        let summary = Self::disasm_summary(&report);
        tool_result(&report, report.success, summary)
    }
    #[tool(description="Read assembler syntax documentation. Topics: index, instructions, directives, expressions, options, disassembler.",output_schema=schema::<ReferenceReport>(),annotations(read_only_hint=true))]
    fn get_reference(&self, Parameters(input): Parameters<ReferenceInput>) -> CallToolResult {
        // Unknown topics are rejected when the arguments are parsed.
        let topic = input.topic.unwrap_or_default();
        let text = topic.text();
        tool_result(
            &ReferenceReport {
                success: true,
                topic: topic.name().to_owned(),
                text: text.to_owned(),
            },
            true,
            text.to_owned(),
        )
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        let mut config = ServerConfig::default();
        config.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        config.server_info = Implementation::new("nesasm-mcp", env!("CARGO_PKG_VERSION"));
        config.instructions = Some(
            concat!(
                "NESASM assembler. Read the syntax reference first (get_reference, or the ",
                "nesasm://reference/* resources); use check to inspect diagnostics before ",
                "assemble. To turn a ROM into source, read the disassembler reference and use ",
                "disassemble, then export_disassembly. All paths are relative to the configured ",
                "project root."
            )
            .into(),
        );
        config
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = ReferenceTopic::ALL
            .iter()
            .map(|topic| {
                Resource::new(
                    format!("{REFERENCE_URI}{}", topic.name()),
                    format!("reference/{}", topic.name()),
                )
                .with_title(format!("NESASM reference: {}", topic.name()))
                .with_description(topic.summary())
                .with_mime_type("text/markdown")
                .with_size(topic.text().len() as u64)
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let topic = request
            .uri
            .strip_prefix(REFERENCE_URI)
            .and_then(|name| name.parse::<ReferenceTopic>().ok())
            .filter(|topic| request.uri == format!("{REFERENCE_URI}{}", topic.name()))
            .ok_or_else(|| {
                ErrorData::resource_not_found(format!("Unknown resource '{}'", request.uri), None)
            })?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(topic.text(), request.uri).with_mime_type("text/markdown"),
        ])
        .into())
    }
}

/// Stdio transport that answers unparsable lines with a JSON-RPC parse error,
/// which the rmcp transport would otherwise drop silently. All output goes
/// through one writer task so responses are never interleaved.
fn stdio_relay() -> (tokio::io::DuplexStream, tokio::io::DuplexStream) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    const PARSE_ERROR: &str =
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}"#;
    let (server_in, mut feed) = tokio::io::duplex(1 << 16);
    let (server_out, drain) = tokio::io::duplex(1 << 16);
    let (send, mut recv) = tokio::sync::mpsc::unbounded_channel::<String>();
    let errors = send.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.trim().is_empty() && serde_json::from_str::<serde_json::Value>(&line).is_err()
            {
                eprintln!("nesasm-mcp: ignoring unparsable message");
                let _ = errors.send(PARSE_ERROR.into());
                continue;
            }
            if feed
                .write_all(format!("{line}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
        // Dropping the feed signals end of input to the server.
    });
    tokio::spawn(async move {
        let mut lines = BufReader::new(drain).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = recv.recv().await {
            if stdout
                .write_all(format!("{line}\n").as_bytes())
                .await
                .is_err()
                || stdout.flush().await.is_err()
            {
                break;
            }
        }
    });
    (server_in, server_out)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut root = None;
    let mut timeout = DEFAULT_TIMEOUT_SECONDS;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                root = Some(PathBuf::from(
                    args.next().ok_or("--root requires a directory")?,
                ))
            }
            "--timeout" => {
                timeout = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|&s| s > 0)
                    .ok_or("--timeout requires a positive number of seconds")?;
            }
            "--help" | "-?" => {
                println!(
                    "nesasm-mcp --root <project-directory> [--timeout <seconds, default {DEFAULT_TIMEOUT_SECONDS}>]"
                );
                return Ok(());
            }
            _ => return Err(format!("Unknown option '{arg}'").into()),
        }
    }
    // Normalized like the paths the core reports (no Windows verbatim prefix).
    let root = root.ok_or("--root is required")?;
    let root = nesasm_core::resolve_path(&root, &root, None)?;
    if !root.is_dir() {
        return Err("Project root must be a directory".into());
    }
    let service = Server::new(root, Duration::from_secs(timeout))
        .serve(stdio_relay())
        .await?;
    service.waiting().await?;
    Ok(())
}

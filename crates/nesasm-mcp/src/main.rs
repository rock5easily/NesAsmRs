use nesasm_core::{
    Artifact, AssembleOptions, AssembleRequest, AssembleResult, BankUsage, Diagnostic,
    DiagnosticCode, LineRecord, ReferenceTopic, Region, SourceLocation, Symbol,
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
/// Largest ROM file inspect_rom reads.
const MAX_ROM_FILE: u64 = 8 * 1024 * 1024;

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
            format!(
                "{}:{}: {}",
                d.location.file.display(),
                d.location.line,
                d.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Server {
    fn relative(root: &Path, path: &Path) -> PathBuf {
        path.strip_prefix(root).unwrap_or(path).to_owned()
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
    #[tool(description="Read assembler syntax documentation. Topics: index, instructions, directives, expressions, options.",output_schema=schema::<ReferenceReport>(),annotations(read_only_hint=true))]
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
                "assemble. All paths are relative to the configured project root."
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

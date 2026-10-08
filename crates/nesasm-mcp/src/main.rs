use nesasm_core::{
    Artifact, AssembleOptions, AssembleRequest, BankUsage, Diagnostic, DiagnosticCode,
    ReferenceTopic, Region, SourceLocation, Symbol,
};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const DEFAULT_TIMEOUT_SECONDS: u64 = 30;

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AssemblyInput {
    /// Entry .asm file, relative to the server project root.
    input: PathBuf,
    /// Extra directories searched by INCLUDE/INCBIN/INCCHR; must be inside the root.
    #[serde(default)]
    include_paths: Vec<PathBuf>,
    #[serde(default)]
    options: AssembleOptions,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BuildInput {
    /// Entry .asm file, relative to the server project root.
    input: PathBuf,
    /// Extra directories searched by INCLUDE/INCBIN/INCCHR; must be inside the root.
    #[serde(default)]
    include_paths: Vec<PathBuf>,
    #[serde(default)]
    options: AssembleOptions,
    /// ROM output path within the project root, ending in .nes or .bin and outside
    /// hidden directories; defaults to input stem + .nes. Listing and S-record
    /// files use the same stem.
    output: Option<PathBuf>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReferenceInput {
    /// Documentation topic (default index).
    topic: Option<ReferenceTopic>,
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
}

impl From<nesasm_core::AssembleResult> for Report {
    fn from(result: nesasm_core::AssembleResult) -> Self {
        Self {
            success: result.success,
            diagnostics: result.diagnostics,
            symbols: result.symbols,
            regions: result.regions,
            banks: result.banks,
            dependencies: result.dependencies,
            artifacts: Vec::new(),
        }
    }
}
#[derive(Serialize, JsonSchema)]
struct ReferenceReport {
    success: bool,
    topic: String,
    text: String,
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
    async fn execute(
        &self,
        input: AssemblyInput,
        write: bool,
        output: Option<PathBuf>,
    ) -> CallToolResult {
        let guard = self.gate.clone().lock_owned().await;
        let root = self.root.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let mut task = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let request = AssembleRequest {
                input: input.input,
                working_directory: root.clone(),
                include_paths: input.include_paths,
                allowed_root: Some(root.clone()),
                options: input.options,
            };
            let (result, artifacts) = if write {
                nesasm_core::build(&request, output.as_deref(), &worker_cancel)
            } else {
                (
                    nesasm_core::assemble_with_cancel(&request, &worker_cancel),
                    Vec::new(),
                )
            };
            let mut report = Report::from(result);
            report.artifacts = artifacts;
            report
        });
        // On timeout the engine stops at its next source line; the gate is held until then.
        let (joined, timed_out) = match tokio::time::timeout(self.timeout, &mut task).await {
            Ok(joined) => (joined, false),
            Err(_) => {
                cancel.store(true, Ordering::Relaxed);
                (task.await, true)
            }
        };
        let mut report = joined.unwrap_or_else(|e| Report {
            success: false,
            diagnostics: vec![Diagnostic::error(
                DiagnosticCode::Internal,
                e.to_string(),
                SourceLocation::default(),
            )],
            ..Report::default()
        });
        if timed_out {
            report.success = false;
            report.artifacts.clear();
            report.diagnostics.push(Diagnostic::error(
                DiagnosticCode::Timeout,
                format!(
                    "Assembly exceeded the {} second time limit",
                    self.timeout.as_secs()
                ),
                SourceLocation::default(),
            ));
        }
        let summary = if report.success {
            format!(
                "Assembly succeeded; {} artifacts, {} symbols",
                report.artifacts.len(),
                report.symbols.len()
            )
        } else {
            report
                .diagnostics
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
        };
        let mut result = CallToolResult::structured(serde_json::to_value(&report).unwrap());
        result.is_error = Some(!report.success);
        result.content = vec![ContentBlock::text(summary)];
        result
    }
    #[tool(description="Assemble a local NESASM project and write ROM, listing and S-record artifacts. Returns diagnostics, symbols and bank/region usage. Paths must stay inside the configured root.",output_schema=schema::<Report>(),annotations(read_only_hint=false,destructive_hint=true))]
    async fn assemble(&self, Parameters(input): Parameters<BuildInput>) -> CallToolResult {
        self.execute(
            AssemblyInput {
                input: input.input,
                include_paths: input.include_paths,
                options: input.options,
            },
            true,
            input.output,
        )
        .await
    }
    #[tool(description="Validate a local NESASM project without writing files. Returns structured diagnostics, symbols, dependencies and bank/region usage.",output_schema=schema::<Report>(),annotations(read_only_hint=true))]
    async fn check(&self, Parameters(input): Parameters<AssemblyInput>) -> CallToolResult {
        self.execute(input, false, None).await
    }
    #[tool(description="Read assembler syntax documentation. Topics: index, instructions, directives, expressions, options.",output_schema=schema::<ReferenceReport>(),annotations(read_only_hint=true))]
    fn get_reference(&self, Parameters(input): Parameters<ReferenceInput>) -> CallToolResult {
        // Unknown topics are rejected when the arguments are parsed.
        let topic = input.topic.unwrap_or_default();
        let text = topic.text();
        let mut result = CallToolResult::structured(
            serde_json::to_value(ReferenceReport {
                success: true,
                topic: topic.name().to_owned(),
                text: text.to_owned(),
            })
            .unwrap(),
        );
        result.is_error = Some(false);
        result.content = vec![ContentBlock::text(text)];
        result
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        let mut config = ServerConfig::default();
        config.capabilities = ServerCapabilities::builder().enable_tools().build();
        config.server_info = Implementation::new("nesasm-mcp", env!("CARGO_PKG_VERSION"));
        config.instructions=Some("NESASM assembler. Start with get_reference; use check to inspect diagnostics before assemble. All paths are relative to the configured project root.".into());
        config
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
    let root = root.ok_or("--root is required")?.canonicalize()?;
    if !root.is_dir() {
        return Err("Project root must be a directory".into());
    }
    let service = Server::new(root, Duration::from_secs(timeout))
        .serve(stdio_relay())
        .await?;
    service.waiting().await?;
    Ok(())
}

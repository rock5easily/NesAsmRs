use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(10);

/// An MCP server process with a fresh project root. The process is killed and
/// the root removed when the client is dropped, even if a test panics.
struct Client {
    server_info: Value,
    child: Child,
    input: ChildStdin,
    /// Server messages; non-protocol output arrives as an error.
    output: mpsc::Receiver<Result<Value, String>>,
    root: PathBuf,
    _dir: tempfile::TempDir,
    id: u64,
}

impl Client {
    /// Starts a server without initializing the session.
    fn spawn() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        let mut child = Command::new(env!("CARGO_BIN_EXE_nesasm-mcp"))
            .arg("--root")
            .arg(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, output) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let message = match line {
                    Ok(text) => serde_json::from_str(&text)
                        .map_err(|_| format!("non-protocol stdout: {text}")),
                    Err(e) => Err(e.to_string()),
                };
                if send.send(message).is_err() {
                    break;
                }
            }
        });
        Self {
            server_info: Value::Null,
            child,
            input,
            output,
            root,
            _dir: dir,
            id: 0,
        }
    }
    /// Starts a server and completes the initialization handshake.
    fn start() -> Self {
        Self::start_with_version("2025-11-25")
    }
    fn start_with_version(version: &str) -> Self {
        let mut c = Self::spawn();
        let init = c.request(
            "initialize",
            json!({"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"integration-test","version":"1"}}),
        );
        assert!(
            init["result"]["capabilities"]["tools"].is_object(),
            "{init}"
        );
        c.server_info = init["result"].clone();
        c.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        c
    }
    fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
    }
    fn next_message(&mut self) -> Value {
        match self.output.recv_timeout(TIMEOUT) {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => panic!("{e}"),
            Err(e) => panic!("MCP response timeout: {e}"),
        }
    }
    /// Waits for the response to `id`, skipping other messages.
    fn response(&mut self, id: &Value) -> Value {
        loop {
            let reply = self.next_message();
            if &reply["id"] == id {
                return reply;
            }
        }
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = json!(self.id);
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        self.response(&id)
    }
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let reply = self.request("tools/call", json!({"name":name,"arguments":arguments}));
        assert!(reply.get("error").is_none(), "{reply}");
        reply["result"].clone()
    }
    fn messages(result: &Value) -> String {
        result["structuredContent"]["diagnostics"].to_string()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn stdio_lifecycle_validation_build_and_recovery() {
    let mut c = Client::start();
    let listed = c.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
    for tool in tools {
        assert!(tool["inputSchema"].is_object());
        assert!(tool["outputSchema"].is_object());
    }
    let reference = c.call("get_reference", json!({"topic":"directives"}));
    assert_eq!(reference["isError"], false);
    assert!(
        reference["structuredContent"]["text"]
            .as_str()
            .unwrap()
            .contains("CATBANK")
    );
    fs::write(
        c.root.join("main.asm"),
        "  .bank 0\n  .org $8000\nReset:\n  lda #$12\n  rts",
    )
    .unwrap();
    let checked = c.call("check", json!({"input":"main.asm"}));
    assert_eq!(checked["isError"], false, "{checked}");
    assert!(!c.root.join("main.nes").exists());
    let built = c.call("assemble", json!({"input":"main.asm"}));
    assert_eq!(built["isError"], false, "{built}");
    assert_eq!(
        built["structuredContent"]["symbols"],
        checked["structuredContent"]["symbols"]
    );
    assert_eq!(
        built["structuredContent"]["banks"],
        checked["structuredContent"]["banks"]
    );
    let bytes = fs::read(c.root.join("main.nes")).unwrap();
    assert_eq!(&bytes[..4], b"NES\x1a");
    assert_eq!(&bytes[16..19], &[0xa9, 0x12, 0x60]);
    let request = nesasm_core::AssembleRequest {
        input: "main.asm".into(),
        working_directory: c.root.clone(),
        include_paths: vec![],
        allowed_root: Some(c.root.clone()),
        options: Default::default(),
        ..Default::default()
    };
    let core = nesasm_core::assemble(&request);
    assert_eq!(&bytes[16..], &core.binary);
    fs::write(c.root.join("main.asm"), "  lda #300").unwrap();
    let failed = c.call("assemble", json!({"input":"main.asm"}));
    assert_eq!(failed["isError"], true);
    assert_eq!(fs::read(c.root.join("main.nes")).unwrap(), bytes);
    fs::write(c.root.join("main.asm"), "  nop").unwrap();
    let recovered = c.call("check", json!({"input":"main.asm"}));
    assert_eq!(recovered["isError"], false);
}

#[test]
fn protocol_errors_identity_and_schemas() {
    let mut c = Client::start();
    assert_eq!(c.server_info["serverInfo"]["name"], "nesasm-mcp");
    // An unparsable line gets a JSON-RPC parse error and the server keeps working.
    writeln!(c.input, "garbage{{").unwrap();
    c.input.flush().unwrap();
    let error = c.next_message();
    assert_eq!(error["error"]["code"], -32700, "{error}");
    assert!(error["id"].is_null());
    let unknown_tool = c.request("tools/call", json!({"name":"nope","arguments":{}}));
    assert_eq!(unknown_tool["error"]["code"], -32602, "{unknown_tool}");
    let unknown_method = c.request("no/such", json!({}));
    assert_eq!(unknown_method["error"]["code"], -32601, "{unknown_method}");
    let unknown_field = c.call("check", json!({"input":"a.asm","bogus":1}));
    assert_eq!(unknown_field["isError"], true, "{unknown_field}");
    let unknown_topic = c.call("get_reference", json!({"topic":"unknown"}));
    assert_eq!(unknown_topic["isError"], true, "{unknown_topic}");
    // String ids are echoed back.
    c.send(json!({"jsonrpc":"2.0","id":"text-id","method":"ping"}));
    let ping = c.response(&json!("text-id"));
    assert!(ping["result"].is_object(), "{ping}");
    let listed = c.request("tools/list", json!({}));
    let schemas = listed["result"]["tools"].to_string();
    assert!(schemas.contains("\"maximum\":3"), "{schemas}");
    assert!(schemas.contains("instructions"), "{schemas}");
}

#[test]
fn session_setup_rules() {
    // Tool calls are rejected before the initialization handshake.
    let mut c = Client::spawn();
    let early = c.request(
        "tools/call",
        json!({"name":"check","arguments":{"input":"a.asm"}}),
    );
    assert!(early.get("error").is_some(), "{early}");
    // An older protocol version is accepted and echoed.
    let c = Client::start_with_version("2025-03-26");
    assert_eq!(c.server_info["protocolVersion"], "2025-03-26");
}

#[test]
fn paths_stay_inside_the_root() {
    let mut c = Client::start();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.bin");
    fs::write(&secret, [1, 2, 3]).unwrap();
    fs::write(c.root.join("main.asm"), "  nop\n").unwrap();
    for (text, what) in [
        ("  .include \"../x.asm\"\n", "relative include"),
        (
            &format!("  .incbin \"{}\"\n", secret.display()),
            "absolute incbin",
        ),
    ] {
        fs::write(c.root.join("probe.asm"), text).unwrap();
        let r = c.call("check", json!({"input":"probe.asm"}));
        assert_eq!(r["isError"], true, "{what}: {r}");
    }
    let r = c.call("check", json!({"input":"../outside.asm"}));
    assert_eq!(r["isError"], true, "{r}");
    fs::write(c.root.join("probe.asm"), "  .include \"x.asm\"\n").unwrap();
    let r = c.call(
        "check",
        json!({"input":"probe.asm","include_paths":[outside.path()]}),
    );
    assert_eq!(r["isError"], true, "include path outside the root: {r}");
    for output in [
        "main.asm",
        "notes.txt",
        ".git/hooks/pre-commit",
        "../escape.nes",
    ] {
        let r = c.call("assemble", json!({"input":"main.asm","output":output}));
        assert_eq!(r["isError"], true, "{output}: {r}");
        assert!(Client::messages(&r).contains("E_OUTPUT"), "{output}: {r}");
    }
    assert_eq!(
        fs::read_to_string(c.root.join("main.asm")).unwrap(),
        "  nop\n"
    );
    assert!(!c.root.join(".git").exists());
    let r = c.call(
        "assemble",
        json!({"input":"main.asm","output":"build/game.nes"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert!(c.root.join("build/game.nes").is_file());
}

#[test]
fn concurrent_calls_all_complete() {
    let mut c = Client::start();
    fs::write(c.root.join("main.asm"), "  nop\n").unwrap();
    let ids = [json!(101), json!(102), json!(103)];
    for id in &ids {
        c.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":"check","arguments":{"input":"main.asm"}}}));
    }
    let mut pending: Vec<&Value> = ids.iter().collect();
    while !pending.is_empty() {
        let reply = c.next_message();
        pending.retain(|id| **id != reply["id"]);
        assert_eq!(reply["result"]["isError"], false, "{reply}");
    }
}

#[test]
fn structured_content_matches_output_schema() {
    let mut c = Client::start();
    fs::write(
        c.root.join("main.asm"),
        "  .bank 0\n  .org $8000\nStart:\n  nop\n",
    )
    .unwrap();
    let listed = c.request("tools/list", json!({}));
    let schemas: Vec<(String, Value)> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap().to_owned(),
                t["outputSchema"].clone(),
            )
        })
        .collect();
    for (name, arguments) in [
        ("check", json!({"input":"main.asm"})),
        ("assemble", json!({"input":"main.asm"})),
        ("check", json!({"input":"missing.asm"})),
        ("get_reference", json!({})),
    ] {
        let result = c.call(name, arguments);
        let content = result["structuredContent"].as_object().unwrap();
        let schema = &schemas.iter().find(|(n, _)| n == name).unwrap().1;
        let properties = schema["properties"].as_object().unwrap();
        for key in content.keys() {
            assert!(properties.contains_key(key), "{name}: {key} not in schema");
        }
        for required in schema["required"].as_array().into_iter().flatten() {
            assert!(
                content.contains_key(required.as_str().unwrap()),
                "{name}: missing {required}"
            );
        }
        for (key, value) in content {
            let expected = &properties[key]["type"];
            let actual = match value {
                Value::Bool(_) => "boolean",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
                Value::Number(_) => "integer",
                Value::Null => "null",
            };
            if let Some(expected) = expected.as_str() {
                assert_eq!(expected, actual, "{name}: {key}");
            }
        }
    }
}

#[test]
fn reference_documents_are_resources() {
    let mut c = Client::start();
    assert!(c.server_info["capabilities"]["resources"].is_object());
    let listed = c.request("resources/list", json!({}));
    let resources = listed["result"]["resources"].as_array().unwrap();
    let uris: Vec<&str> = resources
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert_eq!(
        uris,
        [
            "nesasm://reference/index",
            "nesasm://reference/instructions",
            "nesasm://reference/directives",
            "nesasm://reference/expressions",
            "nesasm://reference/options",
        ]
    );
    for resource in resources {
        assert_eq!(resource["mimeType"], "text/markdown");
        assert!(
            resource["description"]
                .as_str()
                .is_some_and(|d| !d.is_empty())
        );
        let uri = resource["uri"].as_str().unwrap();
        let read = c.request("resources/read", json!({"uri":uri}));
        let contents = &read["result"]["contents"][0];
        assert_eq!(contents["uri"], uri, "{read}");
        // The same text as the get_reference tool.
        let topic = uri.rsplit('/').next().unwrap();
        let tool = c.call("get_reference", json!({"topic":topic}));
        assert_eq!(contents["text"], tool["structuredContent"]["text"]);
        assert_eq!(
            resource["size"].as_u64(),
            Some(contents["text"].as_str().unwrap().len() as u64)
        );
    }
    for uri in [
        "nesasm://reference/unknown",
        "nesasm://reference/INDEX",
        "file:///etc/passwd",
    ] {
        let read = c.request("resources/read", json!({"uri":uri}));
        assert_eq!(read["error"]["code"], -32002, "{uri}: {read}");
    }
    let templates = c.request("resources/templates/list", json!({}));
    assert_eq!(templates["result"]["resourceTemplates"], json!([]));
}

const PROGRAM: &str = concat!(
    "  .inesprg 1\n  .ineschr 0\n",
    "  .bank 1\n  .org $E000\nReset:\n  .include \"lib.asm\"\n",
    "  lda #$12\n  sta <$10\n  bne Reset\nNmi: rti\n",
    "  .org $FFFA\n  .dw Nmi, Reset, 0\n",
);

#[test]
fn in_memory_sources_and_line_map() {
    let mut c = Client::start();
    let r = c.call(
        "check",
        json!({
            "input":"main.asm",
            "sources":{"main.asm":PROGRAM,"lib.asm":"Init:\n  sei\n"},
            "lines":{"file":"main.asm","from_line":7,"to_line":9}
        }),
    );
    assert_eq!(r["isError"], false, "{r}");
    let page = &r["structuredContent"]["lines"];
    assert_eq!(page["total"], 3, "{page}");
    let first = &page["lines"][0];
    assert_eq!(first["file"], "main.asm");
    assert_eq!(
        (first["line"].as_u64(), first["bank"].as_u64()),
        (Some(7), Some(1))
    );
    assert_eq!(first["address"], 0xe001);
    assert_eq!(first["bytes"], "A9 12");
    // Paging and address filters.
    let r = c.call(
        "check",
        json!({"input":"main.asm","sources":{"main.asm":PROGRAM,"lib.asm":"  sei\n"},
               "lines":{"bank":1,"from_address":0xe000,"to_address":0xe003,"offset":1,"limit":1}}),
    );
    let page = &r["structuredContent"]["lines"];
    // Reset:, sei (from lib.asm), lda and sta lie in $E000-$E003 of bank 1.
    assert_eq!(page["total"], 4, "{page}");
    assert_eq!(page["lines"].as_array().unwrap().len(), 1);
    assert_eq!(page["lines"][0]["file"], "lib.asm");
    // Lines are only returned on request, and in-memory sources are never written.
    let r = c.call(
        "check",
        json!({"input":"main.asm","sources":{"main.asm":"  nop\n"}}),
    );
    assert!(r["structuredContent"].get("lines").is_none(), "{r}");
    assert!(!c.root.join("main.asm").exists());
    // assemble writes only the ROM, from in-memory sources.
    let r = c.call(
        "assemble",
        json!({"input":"main.asm","sources":{"main.asm":PROGRAM,"lib.asm":"  sei\n"}}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert!(c.root.join("main.nes").is_file());
    assert!(!c.root.join("main.asm").exists());
    let r = c.call(
        "check",
        json!({"input":"main.asm","sources":{"../escape.asm":"  nop\n"}}),
    );
    assert_eq!(r["isError"], true, "{r}");
}

#[test]
fn inspect_rom_reports_header_vectors_and_code() {
    let mut c = Client::start();
    let project =
        json!({"input":"main.asm","sources":{"main.asm":PROGRAM,"lib.asm":"Init:\n  sei\n"}});
    let r = c.call(
        "inspect_rom",
        json!({"project":project,
               "disassemble":{"bank":1,"address":0xe000,"length":5},
               "hexdump":{"bank":1,"address":0xfffa,"length":6}}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let report = &r["structuredContent"];
    assert_eq!(report["header"]["prg_16k"], 1);
    assert_eq!(report["vectors"][0]["reset"], 0xe000);
    assert_eq!(report["warnings"], json!([]), "{report}");
    let code = report["disassembly"].as_array().unwrap();
    assert_eq!(code[0]["instruction"], "SEI");
    assert_eq!(code[0]["labels"], json!(["Init", "Reset"]));
    assert_eq!(code[3]["instruction"], "BNE $E000");
    assert_eq!(report["hexdump"][0]["bytes"], "07 E0 00 E0 00 00");
    let text = r["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("RESET $E000") && text.contains("E001  A9 12"),
        "{text}"
    );
    // The ROM file written by assemble gives the same vectors, without symbols.
    c.call("assemble", json!(project));
    let r = c.call(
        "inspect_rom",
        json!({"rom":"main.nes","disassemble":{"bank":1,"address":0xe000,"length":1}}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["vectors"][0]["reset"], 0xe000);
    assert_eq!(
        r["structuredContent"]["disassembly"][0]["labels"],
        json!([])
    );
    // Problems are reported, not raised.
    let broken =
        json!({"input":"main.asm","sources":{"main.asm":"  .inesprg 1\n  .org $C000\n  nop\n"}});
    let r = c.call("inspect_rom", json!({"project":broken}));
    let warnings = r["structuredContent"]["warnings"].to_string();
    assert!(
        warnings.contains("RESET") || warnings.contains("vectors"),
        "{r}"
    );
    for bad in [
        json!({}),
        json!({"rom":"main.nes","project":project}),
        json!({"rom":"../outside.nes"}),
        json!({"rom":"main.nes","hexdump":{"bank":9,"address":0}}),
        json!({"project":{"input":"main.asm","sources":{"main.asm":"  lda #300\n"}}}),
    ] {
        let r = c.call("inspect_rom", bad.clone());
        assert_eq!(r["isError"], true, "{bad}: {r}");
    }
}

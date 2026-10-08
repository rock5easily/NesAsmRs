use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

static NEXT_ROOT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct Client {
    server_info: Value,
    child: Child,
    input: ChildStdin,
    output: mpsc::Receiver<Value>,
    root: PathBuf,
    id: u64,
}
impl Client {
    fn start() -> Self {
        let root = std::env::temp_dir().join(format!(
            "nesasm-mcp-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        fs::create_dir_all(&root).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_nesasm-mcp"))
            .arg("--root")
            .arg(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, output) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let text = line.unwrap();
                let value = serde_json::from_str(&text)
                    .unwrap_or_else(|_| panic!("Non-protocol stdout: {text}"));
                if send.send(value).is_err() {
                    break;
                }
            }
        });
        let mut c = Self {
            server_info: Value::Null,
            child,
            input,
            output,
            root,
            id: 0,
        };
        let init=c.request("initialize",json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"integration-test","version":"1"}}));
        assert!(
            init["result"]["capabilities"]["tools"].is_object(),
            "{init}"
        );
        c.server_info = init["result"]["serverInfo"].clone();
        c.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        c
    }
    fn next_message(&mut self) -> Value {
        self.output
            .recv_timeout(Duration::from_secs(10))
            .expect("MCP response timeout")
    }
    fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        loop {
            let reply = self
                .output
                .recv_timeout(Duration::from_secs(10))
                .expect("MCP response timeout");
            if reply["id"] == id {
                return reply;
            }
        }
    }
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let reply = self.request("tools/call", json!({"name":name,"arguments":arguments}));
        assert!(reply.get("error").is_none(), "{reply}");
        reply["result"].clone()
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn stdio_lifecycle_validation_build_and_recovery() {
    let mut c = Client::start();
    let listed = c.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 3);
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
    };
    let core = nesasm_core::assemble(&request);
    assert_eq!(&bytes[16..], &core.binary);
    fs::write(c.root.join("main.asm"), "  lda #300").unwrap();
    let failed = c.call("assemble", json!({"input":"main.asm"}));
    assert_eq!(failed["isError"], true);
    assert_eq!(fs::read(c.root.join("main.nes")).unwrap(), bytes);
    let outside = c.call("check", json!({"input":"../outside.asm"}));
    assert_eq!(outside["isError"], true);
    fs::write(c.root.join("main.asm"), "  nop").unwrap();
    let recovered = c.call("check", json!({"input":"main.asm"}));
    assert_eq!(recovered["isError"], false);
    let escape = c.call(
        "assemble",
        json!({"input":"main.asm","output":"../escape.nes"}),
    );
    assert_eq!(escape["isError"], true);
}

#[test]
fn protocol_errors_identity_and_schemas() {
    let mut c = Client::start();
    assert_eq!(c.server_info["name"], "nesasm-mcp", "{}", c.server_info);
    // An unparsable line gets a JSON-RPC parse error and the server keeps working.
    writeln!(c.input, "garbage{{").unwrap();
    c.input.flush().unwrap();
    let error = c.next_message();
    assert_eq!(error["error"]["code"], -32700, "{error}");
    assert!(error["id"].is_null());
    let ping = c.request("ping", json!({}));
    assert!(ping["result"].is_object(), "{ping}");
    let listed = c.request("tools/list", json!({}));
    let schemas = listed["result"]["tools"].to_string();
    assert!(schemas.contains("\"maximum\":3"), "{schemas}");
    assert!(schemas.contains("instructions"), "{schemas}");
    let reply = c.request(
        "tools/call",
        json!({"name":"get_reference","arguments":{"topic":"unknown"}}),
    );
    assert!(
        reply.get("error").is_some() || reply["result"]["isError"] == true,
        "{reply}"
    );
}

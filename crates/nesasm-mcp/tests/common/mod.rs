//! An MCP client for the integration tests. Each test crate uses a subset.
#![allow(dead_code)]

use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

pub const TIMEOUT: Duration = Duration::from_secs(10);
pub const EXE: &str = env!("CARGO_BIN_EXE_nesasm-mcp");

/// An MCP server process with a fresh project root. When the client is
/// dropped, even if a test panics, the server's input is closed so that it
/// exits normally (it is killed if it does not), and the root is removed.
pub struct Client {
    pub server_info: Value,
    child: Child,
    pub input: Option<ChildStdin>,
    /// Server messages; non-protocol output arrives as an error.
    output: mpsc::Receiver<Result<Value, String>>,
    pub root: PathBuf,
    _dir: tempfile::TempDir,
    id: u64,
}

impl Client {
    /// Starts a server without initializing the session.
    pub fn spawn() -> Self {
        Self::spawn_with(&[])
    }

    /// Starts a server with extra arguments after `--root`.
    pub fn spawn_with(args: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        let mut child = Command::new(EXE)
            .arg("--root")
            .arg(&root)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
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
    pub fn start() -> Self {
        Self::start_with(&[], "2025-11-25")
    }

    pub fn start_with(args: &[&str], version: &str) -> Self {
        let mut c = Self::spawn_with(args);
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

    pub fn send(&mut self, value: Value) {
        self.send_line(&value.to_string());
    }

    pub fn send_line(&mut self, line: &str) {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{line}").unwrap();
        input.flush().unwrap();
    }

    pub fn next_message(&mut self) -> Value {
        match self.output.recv_timeout(TIMEOUT) {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => panic!("{e}"),
            Err(e) => panic!("MCP response timeout: {e}"),
        }
    }

    /// Waits for the response to `id`, skipping other messages.
    pub fn response(&mut self, id: &Value) -> Value {
        loop {
            let reply = self.next_message();
            if &reply["id"] == id {
                return reply;
            }
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = json!(self.id);
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        self.response(&id)
    }

    pub fn call(&mut self, name: &str, arguments: Value) -> Value {
        let reply = self.request("tools/call", json!({"name":name,"arguments":arguments}));
        assert!(reply.get("error").is_none(), "{reply}");
        reply["result"].clone()
    }

    pub fn messages(result: &Value) -> String {
        result["structuredContent"]["diagnostics"].to_string()
    }

    /// The text summary of a tool result.
    pub fn text(result: &Value) -> &str {
        result["content"][0]["text"].as_str().unwrap()
    }

    pub fn write(&self, name: &str, data: impl AsRef<[u8]>) {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, data).unwrap();
    }
}

/// A relative directory, ending in `/`, whose canonical path within `root`
/// stays below 260 characters while a file in it with a name of 12 or more
/// characters exceeds that. On Windows, canonical paths keep the verbatim
/// prefix (`\?\`) only from that length.
pub fn long_dir(root: &Path) -> String {
    let root = std::fs::canonicalize(root).unwrap();
    let root = root.to_string_lossy();
    let length = root.strip_prefix(r"\?\").unwrap_or(&root).len();
    let mut dir = String::new();
    while length + 1 + dir.len() < 250 {
        dir.push(if dir.len() % 50 == 49 { '/' } else { 'd' });
    }
    if dir.ends_with('/') {
        dir.push('d');
    }
    dir.push('/');
    dir
}

impl Client {
    /// Closes the server's input and returns its exit code, or kills it and
    /// returns None if it does not exit within five seconds. End of input
    /// stops the server; a normal exit also lets coverage tools record what
    /// it ran.
    pub fn close(&mut self) -> Option<i32> {
        drop(self.input.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status.code();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        None
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}

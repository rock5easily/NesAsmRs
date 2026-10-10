//! Helpers shared by the CLI tests. Each test crate uses a subset.
#![allow(dead_code)]

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Output, Stdio},
    sync::mpsc,
    time::Duration,
};

pub const EXE: &str = env!("CARGO_BIN_EXE_nesasm");

/// A project directory that is removed when the test ends.
pub struct Project(pub PathBuf, #[allow(dead_code)] tempfile::TempDir);

impl Project {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self(dir.path().to_owned(), dir)
    }

    pub fn write(&self, name: &str, data: impl AsRef<[u8]>) {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, data).unwrap();
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// `nesasm` in the project directory, without the host's NES_INCLUDE.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(EXE);
        command
            .args(args)
            .current_dir(&self.0)
            .env_remove("NES_INCLUDE");
        command
    }

    pub fn run(&self, args: &[&str]) -> Run {
        Run::from(self.command(args).output().unwrap())
    }
}

/// The exit code and text output of one run.
pub struct Run {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        Self {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
            stderr: String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n"),
        }
    }
}

impl Run {
    pub fn success(&self) -> &Self {
        assert_eq!(
            self.code,
            Some(0),
            "stdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    /// A usage error: exit status 2 and the message on stderr.
    pub fn usage_error(&self, message: &str) {
        assert_eq!(self.code, Some(2), "{}", self.stderr);
        assert!(self.stderr.contains(message), "{}", self.stderr);
        assert!(self.stderr.contains("Run 'nesasm --help' for usage."));
        assert!(self.stdout.is_empty(), "{}", self.stdout);
    }
}

pub fn exists(path: impl AsRef<Path>) -> bool {
    path.as_ref().exists()
}

/// A running `-watch` process whose stdout lines can be awaited.
pub struct Watch {
    child: Child,
    pub input: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
}

impl Watch {
    pub fn start(project: &Project, args: &[&str]) -> Self {
        let mut child = project
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            lines,
        }
    }

    /// Waits for a line starting with `prefix` and returns it.
    pub fn expect(&self, prefix: &str) -> String {
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("no line starting with {prefix:?}"));
            if line.starts_with(prefix) {
                return line;
            }
        }
    }

    /// Waits until the watch loop has recorded the state of the watched paths.
    /// The loop prints the H help only after that, so a change made after
    /// this returns is compared against the recorded state.
    pub fn ready(&mut self) {
        self.send("h");
        self.expect("H:");
    }

    pub fn send(&mut self, key: &str) {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{key}").unwrap();
        input.flush().unwrap();
    }

    /// Sends Q and waits for a successful exit.
    pub fn quit(mut self) {
        self.send("q");
        let status = self.child.wait().unwrap();
        assert!(status.success());
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

//! Helpers shared by the integration tests. Each test crate uses a subset.
#![allow(dead_code)]

use nesasm_core::{AssembleOptions, AssembleRequest, AssembleResult};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// A project directory that is removed when the test ends, even on panic.
pub struct Temp(pub PathBuf, #[allow(dead_code)] tempfile::TempDir);

impl Temp {
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

    /// A request for `test.asm`, confined to the project directory.
    pub fn request(&self, options: AssembleOptions) -> AssembleRequest {
        AssembleRequest {
            input: "test.asm".into(),
            working_directory: self.0.clone(),
            allowed_root: Some(self.0.clone()),
            options,
            ..Default::default()
        }
    }

    pub fn run_with(&self, text: &str, options: AssembleOptions) -> AssembleResult {
        self.write("test.asm", text);
        nesasm_core::assemble(&self.request(options))
    }

    pub fn run(&self, text: &str) -> AssembleResult {
        self.run_with(text, AssembleOptions::default())
    }

    /// Assembles `text` and returns the start of the ROM, checking success.
    pub fn bytes(&self, text: &str, len: usize) -> Vec<u8> {
        let r = self.run(text);
        assert!(r.success, "{text}\n{:?}", r.diagnostics);
        r.binary[..len].to_vec()
    }

    /// Assembles `text`, which must fail with an error whose message contains `needle`.
    pub fn fails(&self, text: &str, needle: &str) -> AssembleResult {
        let r = self.run(text);
        assert!(!r.success, "{text} should fail");
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.is_error() && d.message.contains(needle)),
            "{text}: expected an error containing {needle:?}, got {:?}",
            messages(&r)
        );
        r
    }
}

/// A relative directory, ending in `/`, whose canonical path within `root`
/// stays below 260 characters while a file in it with a name of 12 or more
/// characters exceeds that. On Windows, canonical paths keep the verbatim
/// prefix (`\?\`) only from that length.
pub fn long_dir(root: &Path) -> String {
    let root = fs::canonicalize(root).unwrap();
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

pub fn messages(r: &AssembleResult) -> Vec<&str> {
    r.diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.message.as_str())
        .collect()
}

pub fn error_codes(r: &AssembleResult) -> Vec<String> {
    r.diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.code.to_string())
        .collect()
}

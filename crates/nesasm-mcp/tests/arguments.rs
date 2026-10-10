mod common;

use std::process::{Command, Output, Stdio};

fn server(args: &[&str]) -> Output {
    Command::new(common::EXE)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn help_prints_usage() {
    for flag in ["--help", "-?"] {
        let out = server(&[flag]);
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim_end(),
            "nesasm-mcp --root <project-directory> [--timeout <seconds, default 30>]"
        );
    }
}

#[test]
fn invalid_arguments_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let file = dir.path().join("file.txt");
    std::fs::write(&file, "x").unwrap();
    let file = file.to_str().unwrap();
    let missing = dir.path().join("missing");
    let missing = missing.to_str().unwrap();
    let dot_dot = dir.path().join("nope").join("..");
    let dot_dot = dot_dot.to_str().unwrap();
    for (args, message) in [
        (
            &["--root", dot_dot][..],
            "Invalid path: '..' after a directory that does not exist",
        ),
        (&[][..], "--root is required"),
        (&["--root"], "--root requires a directory"),
        (
            &["--root", root, "--timeout"],
            "--timeout requires a positive number of seconds",
        ),
        (
            &["--root", root, "--timeout", "0"],
            "--timeout requires a positive number of seconds",
        ),
        (
            &["--root", root, "--timeout", "x"],
            "--timeout requires a positive number of seconds",
        ),
        (&["--root", root, "--bogus"], "Unknown option '--bogus'"),
        (&["--root", file], "Project root must be a directory"),
        (&["--root", missing], "Project root must be a directory"),
    ] {
        let out = server(args);
        assert!(!out.status.success(), "{args:?}");
        // Nothing but MCP messages may appear on stdout.
        assert!(out.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(message), "{args:?}: {stderr}");
    }
}

#[test]
fn end_of_input_ends_the_server() {
    // Before the handshake there is no session: an error, and no output.
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(common::EXE)
        .args(["--root", dir.path().to_str().unwrap(), "--timeout", "5"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("initialize request"));
    // After it, the server exits normally.
    let mut c = common::Client::start();
    assert_eq!(c.close(), Some(0));
}

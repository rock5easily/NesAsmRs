use std::{
    fs,
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

/// Kills the child process if the test ends before it exits.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn cli_build_failure_and_watch_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("main.asm"),
        "  .bank 0\n  .org $8000\n  .include \"data.asm\"",
    )
    .unwrap();
    fs::write(root.join("data.asm"), "  .db 1").unwrap();
    let exe = env!("CARGO_BIN_EXE_nesasm");
    let check = Command::new(exe)
        .args(["--check", "--json", "main.asm"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(check.status.success());
    assert!(!root.join("main.nes").exists());
    let mut child = KillOnDrop(
        Command::new(exe)
            .args(["-watch", "main.asm"])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut input = child.0.stdin.take().unwrap();
    let stdout = child.0.stdout.take().unwrap();
    let (send, recv) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let wait = || loop {
        let line = recv
            .recv_timeout(Duration::from_secs(10))
            .expect("watch rebuild timeout")
            .expect("watch output");
        if line.starts_with("Assembled") {
            break;
        }
    };
    wait();
    assert_eq!(fs::read(root.join("main.nes")).unwrap()[16], 1);
    // Change length as well as contents so this test works on coarse timestamp filesystems.
    fs::write(root.join("data.asm"), "  .db 2,3").unwrap();
    wait();
    assert_eq!(&fs::read(root.join("main.nes")).unwrap()[16..18], &[2, 3]);
    writeln!(input, "Q").unwrap();
    input.flush().unwrap();
    assert!(child.0.wait().unwrap().success());
    fs::write(root.join("data.asm"), "  .db 300").unwrap();
    let failed = Command::new(exe)
        .args(["main.asm"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_eq!(
        failed.status.code(),
        Some(1),
        "exit status is the error count"
    );
    assert_eq!(&fs::read(root.join("main.nes")).unwrap()[16..18], &[2, 3]);
}

#[test]
fn watch_quits_at_end_of_input_and_usage_errors_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("main.asm"), "  nop\n").unwrap();
    let exe = env!("CARGO_BIN_EXE_nesasm");
    let mut child = KillOnDrop(
        Command::new(exe)
            .args(["-watch", "main.asm"])
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "watch did not quit"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let usage = Command::new(exe).arg("--bogus").output().unwrap();
    assert_eq!(usage.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&usage.stderr).contains("--help"));
}

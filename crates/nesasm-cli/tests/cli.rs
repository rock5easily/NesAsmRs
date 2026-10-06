use std::{
    fs,
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

#[test]
fn cli_build_failure_and_watch_dependencies() {
    let root = std::env::temp_dir().join(format!("nesasm-cli-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("main.asm"),
        "  .bank 0\n  .org $8000\n  .include \"data.asm\"",
    )
    .unwrap();
    fs::write(root.join("data.asm"), "  .db 1").unwrap();
    let exe = env!("CARGO_BIN_EXE_nesasm");
    let check = Command::new(exe)
        .args(["--check", "--json", "main.asm"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(check.status.success());
    assert!(!root.join("main.nes").exists());
    let mut child = Command::new(exe)
        .args(["-watch", "main.asm"])
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (send, recv) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let wait = || loop {
        let line = recv
            .recv_timeout(Duration::from_secs(10))
            .expect("watch rebuild timeout");
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
    assert!(child.wait().unwrap().success());
    fs::write(root.join("data.asm"), "  .db 300").unwrap();
    let failed = Command::new(exe)
        .args(["main.asm"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert_eq!(&fs::read(root.join("main.nes")).unwrap()[16..18], &[2, 3]);
    fs::remove_dir_all(root).unwrap();
}

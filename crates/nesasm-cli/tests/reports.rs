mod common;

use common::{Project, exists};

#[test]
fn diagnostics_and_exit_status() {
    let p = Project::new();
    p.write("t.asm", "  lda #300\n  nop\n  .db 256\n  lda <$100\n");
    let run = p.run(&["t.asm"]);
    // The exit status is the error count.
    assert_eq!(run.code, Some(3));
    let lines: Vec<&str> = run.stderr.lines().collect();
    assert_eq!(lines.len(), 3, "{}", run.stderr);
    let file = p.path("t.asm").canonicalize().unwrap();
    // Diagnostics show plain paths, without the Windows `\\?\` prefix.
    let file = file.display().to_string();
    let file = file.strip_prefix(r"\\?\").unwrap_or(&file);
    assert_eq!(
        lines[0],
        format!("{file}:1: error [E_ASSEMBLY]: Operand size error")
    );
    assert!(lines[1].ends_with(":3: error [E_ASSEMBLY]: Data overflow"));
    assert!(lines[2].ends_with(":4: error [E_ASSEMBLY]: Operand size error"));
    assert!(!run.stdout.contains("Assembled"));
    assert!(!exists(p.path("t.nes")));
    // Errors of the first pass end the assembly before range checks.
    p.write("t.asm", "  lda #300\n  .fail \"stop\"\n");
    let run = p.run(&["t.asm"]);
    assert_eq!(run.code, Some(1));
    assert!(
        run.stderr.ends_with(":2: error [E_ASSEMBLY]: \"stop\"\n"),
        "{}",
        run.stderr
    );
}

#[test]
fn exit_status_is_at_most_255() {
    let p = Project::new();
    p.write("t.asm", "  lda #300\n".repeat(300));
    assert_eq!(p.run(&["t.asm"]).code, Some(255));
}

#[test]
fn success_reports_the_rom_size() {
    let p = Project::new();
    p.write("t.asm", "  .bank 1\n  .db 1\n");
    let run = p.run(&["t.asm"]);
    run.success();
    assert_eq!(run.stdout, "Assembled 16384 bytes\n");
    assert!(run.stderr.is_empty());
}

#[test]
fn check_writes_nothing() {
    let p = Project::new();
    p.write("t.asm", "  .list\n  .db 1\n");
    let run = p.run(&["--check", "t.asm"]);
    run.success();
    assert_eq!(run.stdout, "Assembled 8192 bytes\n");
    assert!(!exists(p.path("t.nes")) && !exists(p.path("t.lst")));
    p.write("t.asm", "  lda #300\n");
    assert_eq!(p.run(&["--check", "t.asm"]).code, Some(1));
}

#[test]
fn json_results() {
    let p = Project::new();
    p.write("t.asm", "Start:\n  .db 1\n");
    let run = p.run(&["--json", "t.asm"]);
    run.success();
    assert!(run.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(json["success"], true);
    assert_eq!(json["symbols"]["Start"]["value"], 0xe000);
    assert!(exists(p.path("t.nes")));
    // Failures are reported in the JSON, not on stderr.
    p.write("t.asm", "  lda #300\n  .db 256\n");
    let run = p.run(&["--json", "--check", "t.asm"]);
    assert_eq!(run.code, Some(2));
    assert!(run.stderr.is_empty(), "{}", run.stderr);
    let json: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(json["success"], false);
    assert_eq!(json["diagnostics"][0]["code"], "E_ASSEMBLY");
    assert_eq!(json["diagnostics"][0]["severity"], "error");
}

#[test]
fn output_errors_are_reported() {
    let p = Project::new();
    p.write("t.asm", "  .db 1\n");
    std::fs::create_dir(p.path("t.nes")).unwrap();
    let run = p.run(&["t.asm"]);
    assert_eq!(run.code, Some(1));
    assert!(
        run.stderr
            .starts_with("t.nes:0: error [E_OUTPUT]: Output path is a directory"),
        "{}",
        run.stderr
    );
}

#[test]
fn region_report() {
    let p = Project::new();
    p.write(
        "t.asm",
        concat!(
            "  .beginregion \"whole\"\n  .ds 300\n  .endregion \"whole\"\n",
            "  .beginregion \"open\"\n",
            "  .endregion \"closed\"\n",
        ),
    );
    let run = p.run(&["t.asm"]);
    run.success();
    assert_eq!(
        run.stdout,
        concat!(
            "Assembled 8192 bytes\n",
            "==================== Region Info ====================\n",
            "Region closed      : BEGINREGION not found\n",
            "Region open        : ENDREGION not found\n",
            "Region whole       :      300 bytes (0x00012C bytes)\n",
            "=====================================================\n",
        )
    );
}

#[test]
fn segment_usage() {
    let p = Project::new();
    p.write(
        "t.asm",
        concat!(
            "  .zp\nzp: .ds 4\n",
            "  .bss\nbuf: .ds $10\n",
            "  .code\n  .bank 0, \"Main\"\n  .org $8000\n  .db 1,2,3\n",
            "  .data\n  .org $8100\n  .db 4\n",
            "  .code\n  .bank 2\n  .org $c000\n  .db 5\n",
        ),
    );
    let run = p.run(&["-s", "t.asm"]);
    run.success();
    let summary = concat!(
        "      ZP    $0000-$0003  [   4]\n",
        "     BSS    $0200-$020F  [  16]\n",
        "\t\t\t\t    USED/FREE\n",
        "BANK   0                    Main       4/8188\n",
        "BANK   1                               0/8192\n",
        "BANK   2                               1/8191\n",
        "\t\t\t\t    ---- ----\n",
        "\t\t\t\t       1K  15K\n",
    );
    assert_eq!(
        run.stdout,
        format!("Assembled 24576 bytes\nsegment usage:\n\n{summary}")
    );
    // -S adds the section runs of each bank.
    let run = p.run(&["-S", "t.asm"]);
    run.success();
    assert!(
        run.stdout.contains(concat!(
            "BANK   0                    Main       4/8188\n",
            "    CODE    $8000-$8002  [   3]\n",
            "    DATA    $8100-$8100  [   1]\n",
            "BANK   1                               0/8192\n",
            "BANK   2                               1/8191\n",
            "    CODE    $C000-$C000  [   1]\n",
        )),
        "{}",
        run.stdout
    );
}

#[test]
fn segment_usage_without_ram_and_after_failure() {
    let p = Project::new();
    p.write("t.asm", "  nop\n");
    let run = p.run(&["-s", "t.asm"]);
    run.success();
    assert!(
        run.stdout.contains(concat!(
            "      ZP    -\n",
            "     BSS    -\n",
            "BANK   0                               1/8191\n",
            "\t\t\t\t    ---- ----\n",
            "\t\t\t\t       1K   7K\n",
        )),
        "{}",
        run.stdout
    );
    // A single bank has no USED/FREE heading.
    assert!(!run.stdout.contains("USED/FREE"));
    // No table after a failure.
    p.write("t.asm", "  lda #300\n");
    let run = p.run(&["-s", "t.asm"]);
    assert_eq!(run.code, Some(1));
    assert!(run.stdout.is_empty(), "{}", run.stdout);
}

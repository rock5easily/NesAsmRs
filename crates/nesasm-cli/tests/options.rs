mod common;

use common::{Project, exists};
use std::fs;

#[test]
fn help_prints_the_options_reference() {
    let p = Project::new();
    for flag in ["-?", "--help"] {
        let run = p.run(&[flag]);
        run.success();
        // The Markdown reference, without heading marks and backquotes.
        assert!(run.stdout.starts_with("CLI options\n"), "{}", run.stdout);
        assert!(run.stdout.contains("nesasm [-options] infile[.asm]"));
        assert!(!run.stdout.contains('`'));
    }
}

#[test]
fn usage_errors_exit_with_status_2() {
    let p = Project::new();
    for (args, message) in [
        (&[][..], "Need input file"),
        (&["a.asm", "b.asm"], "Only one input file is accepted"),
        (&["--bogus", "a.asm"], "Unknown option '--bogus'"),
        (&["a.asm", "--output"], "Missing output path"),
        (&["a.asm", "--include"], "Missing include path"),
        (&["a.asm", "-e"], "Missing encoding"),
        (&["a.asm", "-e", "latin1"], "Encoding must be UTF8 or SJIS"),
        (&["a.asm", "-l"], "Missing listing level"),
        (&["a.asm", "-l", "x"], "Invalid listing level"),
        (
            &["--json", "-watch", "a.asm"],
            "--json cannot be combined with -watch",
        ),
    ] {
        p.run(args).usage_error(message);
    }
}

#[test]
fn input_extension_and_output_path() {
    let p = Project::new();
    p.write("game.asm", "  .db 1\n");
    // `.asm` is added to the input name.
    p.run(&["game"]).success();
    assert_eq!(fs::read(p.path("game.nes")).unwrap().len(), 16 + 8192);
    p.run(&["game.asm", "--output", "out/rom.bin"]).success();
    assert_eq!(fs::read(p.path("out/rom.bin")).unwrap()[16], 1);
}

#[test]
fn raw_srec_and_autozp() {
    let p = Project::new();
    p.write("t.asm", "  .inesprg 1\n  lda $12\n");
    p.run(&["-raw", "-autozp", "t.asm"]).success();
    // No header, and AUTOZP picks the zero page form.
    assert_eq!(&fs::read(p.path("t.nes")).unwrap()[..2], &[0xa5, 0x12]);
    fs::remove_file(p.path("t.nes")).unwrap();
    p.run(&["-srec", "t.asm"]).success();
    assert!(!exists(p.path("t.nes")));
    let srec = fs::read_to_string(p.path("t.s28")).unwrap();
    assert!(srec.starts_with("S2"), "{srec}");
}

#[test]
fn listing_levels_and_macro_listing() {
    let p = Project::new();
    // Without .LIST there is no listing file at any level.
    p.write("t.asm", "  nop\n");
    p.run(&["-l3", "t.asm"]).success();
    assert!(!exists(p.path("t.lst")));
    p.write("t.asm", "m .macro\n  .db $5a\n  .endm\n  .list\n  m\n");
    let listing = |args: &[&str]| {
        let _ = fs::remove_file(p.path("t.lst"));
        p.run(args).success();
        fs::read_to_string(p.path("t.lst")).ok()
    };
    // Level 0 (also `-l` below 0) writes no listing.
    assert_eq!(listing(&["-l0", "t.asm"]), None);
    assert_eq!(listing(&["-l", "-1", "t.asm"]), None);
    for args in [
        &["t.asm"][..],
        &["-l1", "t.asm"],
        &["-l2", "t.asm"],
        &["-l3", "t.asm"],
        &["-l", "99", "t.asm"],
    ] {
        let text = listing(args).unwrap_or_else(|| panic!("{args:?}"));
        assert!(text.contains("  m"), "{args:?}: {text}");
    }
    // -m lists the macro body.
    assert!(!listing(&["t.asm"]).unwrap().contains(".db $5a"));
    assert!(listing(&["-m", "t.asm"]).unwrap().contains(".db $5a"));
}

#[test]
fn sjis_sources() {
    let p = Project::new();
    // `  .db "あ"` in Shift_JIS.
    p.write("t.asm", b"  .db \"\x82\xa0\"\n");
    let run = p.run(&["t.asm"]);
    assert_eq!(run.code, Some(1));
    assert!(run.stderr.contains("Invalid UTF-8"), "{}", run.stderr);
    for encoding in ["SJIS", "sjis"] {
        p.run(&["-e", encoding, "t.asm"]).success();
        assert_eq!(&fs::read(p.path("t.nes")).unwrap()[16..18], &[0x82, 0xa0]);
    }
    p.write("u.asm", "  .db \"あ\"\n");
    p.run(&["-e", "utf8", "u.asm"]).success();
}

#[test]
fn include_directories_from_options_and_environment() {
    let p = Project::new();
    p.write("lib/a.asm", "  .db 1\n");
    p.write("env/b.asm", "  .db 2\n");
    p.write("t.asm", "  .include \"a.asm\"\n  .include \"b.asm\"\n");
    let run = p.run(&["t.asm"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    let run = p
        .command(&["--include", "lib", "t.asm"])
        .env("NES_INCLUDE", "env")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(&fs::read(p.path("t.nes")).unwrap()[16..18], &[1, 2]);
}

#[test]
fn only_ten_nes_include_directories_are_used() {
    let p = Project::new();
    let dirs: Vec<String> = (0..11).map(|i| format!("d{i}")).collect();
    p.write("d10/late.asm", "  .db 1\n");
    p.write("t.asm", "  .include \"late.asm\"\n");
    let output = p
        .command(&["t.asm"])
        .env("NES_INCLUDE", dirs.join(";"))
        .output()
        .unwrap();
    let run = common::Run::from(output);
    assert!(
        run.stderr
            .contains("warning: NES_INCLUDE has 11 directories; only the first 10 are used"),
        "{}",
        run.stderr
    );
    // The eleventh directory is not searched.
    assert_eq!(run.code, Some(1));
    assert!(
        run.stderr.contains("Cannot open file 'late.asm'"),
        "{}",
        run.stderr
    );
}

#[test]
fn warnings_can_be_disabled() {
    let p = Project::new();
    p.write("big.bin", vec![0u8; 0x2010]);
    p.write("t.asm", "  .opt w+\n  .incbin \"big.bin\"\n");
    let run = p.run(&["t.asm"]);
    run.success();
    assert!(
        run.stderr
            .contains("t.asm:2: warning [W_BANK_OVERFLOW]: Bank overflow by 16 bytes"),
        "{}",
        run.stderr
    );
    let run = p.run(&["-wd", "t.asm"]);
    run.success();
    assert!(run.stderr.is_empty(), "{}", run.stderr);
}

#[test]
fn forward_if_compatibility_is_opt_in() {
    let p = Project::new();
    p.write(
        "t.asm",
        "  .if later\n  .db 1\n  .else\n  .db 2\n  .endif\nlater = 1\n",
    );
    let run = p.run(&["t.asm"]);
    assert!(run.code.is_some_and(|code| code > 0));
    assert!(run.stderr.contains("IF condition changed between passes"));
    assert!(!p.path("t.nes").exists());
    let run = p.run(&["--compat-if", "t.asm"]);
    run.success();
    assert!(
        run.stderr
            .contains("t.asm:1: warning [W_UNDEFINED_IF]: Undefined label in IF condition."),
        "{}",
        run.stderr
    );
    assert_eq!(fs::read(p.path("t.nes")).unwrap()[16], 1);
    let run = p.run(&["--compat-if", "-wd", "t.asm"]);
    run.success();
    assert!(run.stderr.is_empty());
}

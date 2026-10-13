mod common;

use common::Client;
use serde_json::{Value, json};
use std::path::Path;

fn project(source: &str) -> Value {
    json!({"input":"main.asm","sources":{"main.asm":source}})
}

#[test]
fn summaries_of_tool_results() {
    let mut c = Client::start();
    let r = c.call("check", project("Start:\n  nop\n"));
    assert_eq!(r["isError"], false);
    let symbols = r["structuredContent"]["symbols"].as_object().unwrap().len();
    assert_eq!(
        Client::text(&r),
        format!("Assembly succeeded; 0 artifacts, {symbols} symbols")
    );
    // A listing is a second artifact.
    let r = c.call("assemble", project("  .list\n  nop\n"));
    assert!(
        Client::text(&r).starts_with("Assembly succeeded; 2 artifacts"),
        "{r}"
    );
    let kinds: Vec<&Value> = r["structuredContent"]["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| &a["kind"])
        .collect();
    assert_eq!(kinds, [&json!("rom"), &json!("listing")]);
    // Failures list each error as file:line: message.
    let r = c.call("check", project("  nop\n  lda #300\n"));
    assert_eq!(r["isError"], true);
    assert!(
        Client::text(&r).ends_with("main.asm:2: Operand size error"),
        "{r}"
    );
}

#[test]
fn srec_artifacts_and_include_paths() {
    let mut c = Client::start();
    c.write("lib/defs.asm", "VALUE = 7\n");
    let r = c.call(
        "assemble",
        json!({"input":"main.asm","sources":{"main.asm":"  .include \"defs.asm\"\n  .db VALUE\n"},
               "include_paths":["lib"],"options":{"srec":true},"output":"out/game.nes"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let artifact = &r["structuredContent"]["artifacts"][0];
    assert_eq!(artifact["kind"], "srec");
    assert!(c.root.join("out/game.s28").is_file());
    assert!(!c.root.join("out/game.nes").exists());
}

#[test]
fn sjis_in_memory_sources() {
    let mut c = Client::start();
    let sjis = json!({"input":"main.asm","sources":{"main.asm":"  .db \"あ\"\n"},
                      "options":{"encoding":"sjis"}});
    // The text is encoded as SJIS before assembly, so the string is two bytes.
    let r = c.call(
        "inspect_rom",
        json!({"project":sjis,"hexdump":{"bank":0,"address":0xe000,"length":2}}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["hexdump"][0]["bytes"], "82 A0");
    // A character SJIS cannot encode is an E_IO error naming the source.
    let emoji = json!({"input":"main.asm","sources":{"main.asm":"  .db \"\u{1F600}\"\n"},
                       "options":{"encoding":"sjis"}});
    for (tool, arguments) in [
        ("check", emoji.clone()),
        ("inspect_rom", json!({"project":emoji})),
    ] {
        let r = c.call(tool, arguments);
        assert_eq!(r["isError"], true, "{tool}: {r}");
        let d = &r["structuredContent"]["diagnostics"][0];
        assert_eq!(d["code"], "E_IO", "{tool}: {r}");
        assert_eq!(
            d["message"], "main.asm: Source cannot be encoded as SJIS",
            "{tool}"
        );
    }
}

#[test]
fn line_map_details() {
    let mut c = Client::start();
    let source = concat!(
        "  .zp\nvar: .ds 1\n  .code\n",
        "emit .macro\n  .db \\1\n  .endm\n",
        "  emit 5\n",
        "  .ds 100, 1\n",
    );
    let r = c.call(
        "check",
        json!({"input":"main.asm","sources":{"main.asm":source},"lines":{}}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let lines = r["structuredContent"]["lines"]["lines"].as_array().unwrap();
    let at = |line: u64| lines.iter().find(|l| l["line"] == line).unwrap();
    // Zero page lines have no bank.
    assert!(at(2).get("bank").is_none(), "{}", at(2));
    // The macro body line names its call site.
    let body = at(5);
    assert_eq!(body["called_from"], "main.asm:7");
    assert_eq!(body["bytes"], "05");
    // Large data shows its first 64 bytes and the full size.
    let fill = at(8);
    assert_eq!(fill["size"], 100);
    assert_eq!(fill["bytes"].as_str().unwrap().split(' ').count(), 64);
}

#[test]
fn long_paths_within_the_root() {
    // Existing files whose canonical paths are long, in a directory whose
    // path is not: on Windows, only the files' paths keep the `\\?\` prefix.
    let mut c = Client::start();
    let dir = common::long_dir(&c.root);
    c.write(&format!("{dir}included_file.asm"), "  .db 2\n");
    c.write(
        &format!("{dir}source_file.asm"),
        "  .include \"included_file.asm\"\n",
    );
    let arguments = json!({"input": format!("{dir}source_file.asm"), "include_paths": [dir],
                           "output": format!("{dir}output_file.nes"), "lines": {}});
    // The second build overwrites the ROM.
    for _ in 0..2 {
        let r = c.call("assemble", arguments.clone());
        assert_eq!(r["isError"], false, "{r}");
        // Lines name their files relative to the root.
        let file = r["structuredContent"]["lines"]["lines"][0]["file"]
            .as_str()
            .unwrap();
        assert_eq!(
            Path::new(file),
            Path::new(&format!("{dir}included_file.asm"))
        );
    }
}

#[test]
fn line_pages_are_limited() {
    let mut c = Client::start();
    let source = "  .db 0\n".repeat(2100);
    let page = |c: &mut Client, query: Value| {
        let r = c.call(
            "check",
            json!({"input":"main.asm","sources":{"main.asm":source},"lines":query}),
        );
        r["structuredContent"]["lines"].clone()
    };
    let default = page(&mut c, json!({}));
    assert_eq!(default["total"], 2100);
    assert_eq!(default["lines"].as_array().unwrap().len(), 200);
    let most = page(&mut c, json!({"limit":5000}));
    assert_eq!(most["lines"].as_array().unwrap().len(), 2000);
    let last = page(&mut c, json!({"offset":2050,"limit":100}));
    assert_eq!(last["offset"], 2050);
    assert_eq!(last["lines"].as_array().unwrap().len(), 50);
}

/// A program that takes far longer than a second to assemble (tens of seconds
/// in a debug build): about 910,000 expanded lines, below the line limit,
/// most with a long expression.
fn slow_program() -> String {
    let expression = vec!["1"; 100].join("+");
    let mut text = String::from("m0 .macro\n");
    text.push_str(&format!("X = {expression}\n").repeat(8));
    text.push_str("  .endm\n");
    for level in 1..=5 {
        text.push_str(&format!("m{level} .macro\n"));
        text.push_str(&format!("  m{}\n", level - 1).repeat(10));
        text.push_str("  .endm\n");
    }
    text.push_str("  m5\n");
    text
}

#[test]
fn calls_stop_at_the_time_limit() {
    let mut c = Client::start_with(&["--timeout", "1"], "2025-11-25");
    let slow = project(&slow_program());
    for (tool, arguments) in [
        ("check", slow.clone()),
        ("inspect_rom", json!({"project":slow})),
    ] {
        let started = std::time::Instant::now();
        let r = c.call(tool, arguments);
        assert_eq!(r["isError"], true, "{tool}: {r}");
        let d = &r["structuredContent"]["diagnostics"][0];
        assert_eq!(d["code"], "E_TIMEOUT", "{tool}: {r}");
        assert_eq!(d["message"], "Assembly exceeded the 1 second time limit");
        assert!(started.elapsed() < std::time::Duration::from_secs(8));
    }
    // The server keeps working.
    let r = c.call("check", project("  nop\n"));
    assert_eq!(r["isError"], false, "{r}");
}

#[test]
fn inspect_rom_files() {
    let mut c = Client::start();
    // A raw payload has no header; the summary says so and warns.
    c.write("raw.bin", vec![0xea; 0x2000]);
    let r = c.call("inspect_rom", json!({"rom":"raw.bin"}));
    assert_eq!(r["isError"], false, "{r}");
    let text = Client::text(&r);
    assert!(text.starts_with("No iNES header; 1 banks"), "{text}");
    assert!(
        text.contains("Warning: No iNES header (raw payload)"),
        "{text}"
    );
    // Files that cannot be inspected.
    let mut trainer = b"NES\x1a\x01\x00\x04\x00".to_vec();
    trainer.resize(16, 0);
    c.write("trainer.nes", &trainer);
    c.write("big.nes", vec![0u8; 8 * 1024 * 1024 + 1]);
    for (rom, message) in [
        ("trainer.nes", "File ends inside the trainer"),
        ("big.nes", "ROM file exceeds 8 MiB"),
        ("missing.nes", ""),
    ] {
        let r = c.call("inspect_rom", json!({"rom":rom}));
        assert_eq!(r["isError"], true, "{rom}: {r}");
        let d = &r["structuredContent"]["diagnostics"][0];
        assert_eq!(d["code"], "E_IO", "{rom}");
        assert!(
            d["message"].as_str().unwrap().contains(message),
            "{rom}: {r}"
        );
    }
}

#[test]
fn inspect_rom_ranges_and_cmos_forms() {
    let mut c = Client::start();
    let source = format!("  .org $8000\n  lda [$10]\n{}", "  nop\n".repeat(600));
    let inspect = |c: &mut Client, ranges: Value| {
        let mut arguments = json!({"project":project(&source)});
        for (key, value) in ranges.as_object().unwrap() {
            arguments[key] = value.clone();
        }
        c.call("inspect_rom", arguments)
    };
    // Defaults: 256 bytes of dump and 32 instructions.
    let r = inspect(
        &mut c,
        json!({"hexdump":{"bank":0,"address":0x8000},"disassemble":{"bank":0,"address":0x8000}}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let report = &r["structuredContent"];
    assert_eq!(report["hexdump"].as_array().unwrap().len(), 16);
    assert_eq!(report["disassembly"].as_array().unwrap().len(), 32);
    // 65C02-only forms are marked in the text.
    assert!(
        Client::text(&r).contains("8000  B2 10     LDA [$10]  ; 65C02 only"),
        "{}",
        Client::text(&r)
    );
    // Lengths are capped at 4096 bytes and 512 instructions.
    let r = inspect(
        &mut c,
        json!({"hexdump":{"bank":0,"address":0x8000,"length":100000},
               "disassemble":{"bank":0,"address":0x8000,"length":100000}}),
    );
    let report = &r["structuredContent"];
    assert_eq!(report["hexdump"].as_array().unwrap().len(), 4096 / 16);
    assert_eq!(report["disassembly"].as_array().unwrap().len(), 512);
    // A bank that does not exist.
    let r = inspect(&mut c, json!({"disassemble":{"bank":9,"address":0x8000}}));
    assert_eq!(r["isError"], true, "{r}");
    assert!(
        Client::messages(&r).contains("Bank 9 does not exist; the ROM has 1 banks"),
        "{r}"
    );
}

#[test]
fn forward_if_compatibility_is_an_assembly_option() {
    let mut c = Client::start();
    let source = "  .if later\n  .db 1\n  .else\n  .db 2\n  .endif\nlater = 1\n";
    let r = c.call("check", project(source));
    assert_eq!(r["isError"], true);
    assert!(Client::messages(&r).contains("IF condition changed between passes"));
    let mut input = project(source);
    input["options"] = json!({"compat_if":true});
    for tool in ["check", "assemble"] {
        let r = c.call(tool, input.clone());
        assert_eq!(r["isError"], false, "{r}");
        let d = &r["structuredContent"]["diagnostics"][0];
        assert_eq!(d["severity"], "warning");
        assert_eq!(d["code"], "W_UNDEFINED_IF");
        assert_eq!(d["message"], "Undefined label in IF condition.");
    }
    assert_eq!(std::fs::read(c.root.join("main.nes")).unwrap()[16], 1);
    let r = c.call(
        "inspect_rom",
        json!({"project":input,"hexdump":{"bank":0,"address":0xe000,"length":1}}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["hexdump"][0]["bytes"], "01");
}

/// Builds `old/game.nes` (16 KiB PRG, 8 KiB CHR) and returns its bytes.
fn game_rom(c: &mut Client) -> Vec<u8> {
    let source = concat!(
        "  .inesprg 1\n  .ineschr 1\n  .inesmir 1\n  .bank 0\n  .org $C000\n",
        "Reset:\n  lda #0\n  sta $2000\n  jsr Wait\nLoop:\n  jmp Loop\n",
        "Wait:\n  lda $2002\n  bpl Wait\n  rts\nNmi:\n  inc <$10\n  rti\n",
        "  .bank 1\n  .org $FFFA\n  .dw Nmi, Reset, Nmi\n",
        "  .bank 2\n  .db 1, 2, 3\n",
    );
    let r = c.call(
        "assemble",
        json!({"input":"src/game.asm","sources":{"src/game.asm":source},"output":"old/game.nes"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    std::fs::read(c.root.join("old/game.nes")).unwrap()
}

#[test]
fn disassemble_reports_and_queries_without_writing() {
    let mut c = Client::start();
    game_rom(&mut c);
    c.write(
        "game.disasm.json",
        r#"{"rom": "old/game.nes", "labels": [{"address": "$C00B", "name": "WaitVBlank"}],
            "symbols": [{"name": "frames", "value": "$10", "comment": "frame counter"}]}"#,
    );
    let r = c.call(
        "disassemble",
        json!({"annotations":"game.disasm.json","view":{"from":"$C000","to":"0xC00F"},
               "xrefs":["WaitVBlank","PPUSTATUS","nothing"],"routines":{},"ram":{"limit":1}}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let report = &r["structuredContent"];
    assert_eq!(report["verification"]["matched"], true, "{r}");
    let files = report["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(
        std::path::Path::new(files[0].as_str().unwrap()),
        std::path::Path::new("old/game.asm")
    );
    assert_eq!(report["summary"]["format"], "nes");
    assert_eq!(report["summary"]["banks"][0]["org"], "$C000");
    assert_eq!(report["summary"]["banks"][2]["chr"], true);
    let view = report["view"]["items"].as_array().unwrap();
    assert_eq!(view[0]["text"], "RESET:");
    assert_eq!(view[1]["bytes"], "A9 00");
    assert_eq!(view[1]["text"], "\tlda #$00");
    let xrefs = &report["xrefs"];
    assert_eq!(xrefs["WaitVBlank"].as_array().unwrap().len(), 2);
    assert_eq!(xrefs["PPUSTATUS"][0]["line"], "lda PPUSTATUS");
    // An unknown name is a warning; the rest of the report stands.
    let warning = &report["diagnostics"][0];
    assert_eq!(
        (&warning["severity"], &warning["code"]),
        (&json!("warning"), &json!("E_DISASM"))
    );
    let ram = &report["ram"];
    assert_eq!(
        (
            ram["total"].as_u64(),
            ram["items"].as_array().unwrap().len()
        ),
        (Some(3), 1)
    );
    let text = Client::text(&r);
    let lda = format!("$C000  {:<27}   lda #$00", "A9 00");
    for line in [
        "nes ROM: 2 PRG and 1 CHR banks; 9 instructions, 4 labels, 3 routines",
        "Bank 0 $C000: code 20, data 0, unknown 8172 bytes",
        "Round trip: the source reassembles to the same ROM",
        &lda,
        "warning: 'nothing' is neither a label, a symbol nor an address",
        "References to WaitVBlank: 2",
        "  bank 0 $C005 in RESET: jsr WaitVBlank",
        "  WaitVBlank bank 0 $C00B: 6 bytes, 1 callers; calls []; reads [PPUSTATUS]; writes []",
        "RAM: 1 of 3 from 0",
    ] {
        assert!(
            text.lines().any(|l| l == line),
            "missing {line:?} in\n{text}"
        );
    }
    assert!(!c.root.join("old/game.asm").exists());
    assert!(!c.root.join("old/game.chr").exists());
}

#[test]
fn disassemble_rejects_bad_requests() {
    let mut c = Client::start();
    game_rom(&mut c);
    c.write("broken.json", "{\"rom\": ");
    c.write("norom.json", "{}");
    for (arguments, code, message) in [
        (
            json!({"annotations":"broken.json"}),
            "E_DISASM",
            "Invalid annotations",
        ),
        (
            json!({"annotations":"norom.json"}),
            "E_DISASM",
            "must name the ROM file",
        ),
        (
            json!({"annotations":"norom.json","inline":{"rom":"old/game.nes"}}),
            "E_DISASM",
            "Give either annotations",
        ),
        (
            json!({"inline":{"rom":"missing.nes"}}),
            "E_IO",
            "missing.nes",
        ),
        (
            json!({"inline":{"rom":"../outside.nes"}}),
            "E_IO",
            "outside.nes",
        ),
        (
            json!({"inline":{"rom":"old/game.nes"},"output":"game.txt"}),
            "E_OUTPUT",
            "Source output must have a .asm extension",
        ),
        (
            json!({"inline":{"rom":"old/game.nes","labels":[{"address":"$C000","name":"9"}]}}),
            "E_DISASM",
            "Invalid name '9'",
        ),
        (
            json!({"inline":{"rom":"old/game.nes","options":{"mapper4_fixed_prg_16k":true}}}),
            "E_DISASM",
            "options.mapper4_fixed_prg_16k applies only to mapper 4 (MMC3); the ROM uses mapper 0",
        ),
    ] {
        let r = c.call("disassemble", arguments.clone());
        assert_eq!(r["isError"], true, "{arguments}: {r}");
        let d = &r["structuredContent"]["diagnostics"][0];
        assert_eq!(d["code"], code, "{arguments}: {r}");
        assert!(
            d["message"].as_str().unwrap().contains(message),
            "{arguments}: {r}"
        );
    }
    // Errors in an annotation file name it.
    let r = c.call("disassemble", json!({"annotations":"broken.json"}));
    let location = &r["structuredContent"]["diagnostics"][0]["location"];
    assert_eq!(location["file"], "broken.json");
}

#[test]
fn export_writes_verified_sources_and_keeps_edits() {
    let mut c = Client::start();
    let original = game_rom(&mut c);
    let export = |c: &mut Client, output: Option<&str>| {
        let mut arguments = json!({"inline":{"rom":"old/game.nes"}});
        if let Some(output) = output {
            arguments["output"] = json!(output);
        }
        c.call("export_disassembly", arguments)
    };
    let r = export(&mut c, None);
    assert_eq!(r["isError"], false, "{r}");
    let kinds: Vec<&Value> = r["structuredContent"]["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| &a["kind"])
        .collect();
    assert_eq!(kinds, [&json!("source"), &json!("chr")]);
    assert!(Client::text(&r).contains("Wrote "), "{r}");
    let source = std::fs::read_to_string(c.root.join("old/game.asm")).unwrap();
    assert!(source.starts_with("; Generated by nesasm disassembler; checksum "));
    assert!(
        source.contains("\t.incbin \"old/game.chr\", 0, 8192"),
        "{source}"
    );
    // The written source assembles to the original ROM.
    let r = c.call(
        "assemble",
        json!({"input":"old/game.asm","output":"rebuilt/game.nes"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        std::fs::read(c.root.join("rebuilt/game.nes")).unwrap(),
        original
    );
    // An unedited generated source is replaced; an edited one is kept.
    let r = export(&mut c, None);
    assert_eq!(r["isError"], false, "{r}");
    c.write("old/game.asm", source.replace("lda #$00", "lda #0"));
    c.write("hand.asm", "  nop\n");
    for (output, message) in [
        (None, "exists and is not an unedited generated file"),
        (
            Some("hand.asm"),
            "exists and is not an unedited generated file",
        ),
        (
            Some(".git/game.asm"),
            "must not contain hidden files or directories",
        ),
        (Some("../game.asm"), "outside the project root"),
    ] {
        let r = export(&mut c, output);
        assert_eq!(r["isError"], true, "{output:?}: {r}");
        assert!(Client::messages(&r).contains(message), "{output:?}: {r}");
    }
    assert_eq!(
        std::fs::read_to_string(c.root.join("hand.asm")).unwrap(),
        "  nop\n"
    );
    assert!(!c.root.join(".git").exists());
    // A source that does not reassemble to the ROM is not written.
    let mut nes2 = original.clone();
    nes2[7] = 0x08;
    c.write("nes2.nes", &nes2);
    let r = c.call("export_disassembly", json!({"inline":{"rom":"nes2.nes"}}));
    assert_eq!(r["isError"], true, "{r}");
    assert!(
        Client::messages(&r).contains("does not reassemble to the ROM; nothing was written"),
        "{r}"
    );
    assert!(
        Client::text(&r).contains(
            "Round trip FAILED: 1 bytes differ; first header $0007 expected 08 actual 00"
        ),
        "{}",
        Client::text(&r)
    );
    assert!(!c.root.join("nes2.asm").exists());
}

#[test]
fn rom_hacks_become_patch_sources() {
    let mut c = Client::start();
    let body = "Reset:\n  jsr Init\nLoop:\n  jmp Loop\nInit:\n  rts\nNmi:\n  rti\n";
    let hook = "  .org $E100\nNewInit:\n  lda #1\n  jmp Init\n";
    for (name, code) in [
        ("base", body.to_owned()),
        ("hack", body.replace("jsr Init", "jsr NewInit") + hook),
    ] {
        let source = format!(
            "  .inesprg 1\n  .bank 1\n  .org $E000\n{code}  .org $FFFA\n  .dw Nmi, Reset, Nmi\n"
        );
        let r = c.call(
            "assemble",
            json!({"input":"src.asm","sources":{"src.asm":source},"output":format!("roms/{name}.nes")}),
        );
        assert_eq!(r["isError"], false, "{r}");
    }
    c.write(
        "hack.disasm.json",
        r#"{"rom": "roms/hack.nes", "base": "roms/base.nes"}"#,
    );
    let r = c.call("disassemble", json!({"annotations":"hack.disasm.json"}));
    assert_eq!(r["isError"], false, "{r}");
    let report = &r["structuredContent"];
    assert_eq!(report["verification"]["matched"], true, "{r}");
    assert_eq!(report["summary"]["diff"]["ranges"], 2);
    let diffs = &report["diffs"]["items"];
    assert_eq!(diffs[1]["start"], "$E100");
    assert_eq!(diffs[1]["base_fill"], "00");
    assert_eq!(diffs[1]["referenced_from"][0]["line"], "jsr sub_E100");
    let text = Client::text(&r);
    for line in [
        "ROM hack: 7 bytes differ from the base ROM; 2 ranges (8 bytes) are source, the rest is included from the base",
        "Changed ranges: 2",
        "  bank 1 $E000-$E002: 2 changed; code 3, data 0, unknown 0; labels [RESET]; in [RESET]; from [bank 1 $FFFC pointer]",
        "  bank 1 $E100-$E104: 5 changed; code 5, data 0, unknown 0; base was free space (00); labels [sub_E100]; in [sub_E100]; from [bank 1 $E000 jsr sub_E100]",
    ] {
        assert!(
            text.lines().any(|l| l == line),
            "missing {line:?} in\n{text}"
        );
    }
    // The exported source rebuilds the hack from the base ROM.
    let r = c.call(
        "export_disassembly",
        json!({"annotations":"hack.disasm.json"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let source = std::fs::read_to_string(c.root.join("roms/hack.asm"))
        .unwrap()
        .replace("\r\n", "\n");
    // The whole base once (without its header), then the ranges over it.
    assert!(
        source.contains("\t.bank 0\n\t.org $C000\n\t.incbin \"roms/base.nes\", 16, 16384\n"),
        "{source}"
    );
    assert_eq!(source.matches(".incbin").count(), 1, "{source}");
    assert!(source.contains("\t.org $E100\n"), "{source}");
    let r = c.call(
        "assemble",
        json!({"input":"roms/hack.asm","output":"rebuilt.nes"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        std::fs::read(c.root.join("rebuilt.nes")).unwrap(),
        std::fs::read(c.root.join("roms/hack.nes")).unwrap()
    );
    // The base ROM must be inside the root.
    let r = c.call(
        "disassemble",
        json!({"inline":{"rom":"roms/hack.nes","base":"../base.nes"}}),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert_eq!(r["structuredContent"]["diagnostics"][0]["code"], "E_IO");
}

#[test]
fn mapper4_fixed_prg_16k_writes_the_last_two_banks_together() {
    let mut c = Client::start();
    // `lda $0390,x` at $DFFE ends in bank 3.
    let source = concat!(
        "  .inesprg 2\n  .inesmap 4\n  .bank 2\n  .org $DFFE\nReset:\n  .db $BD, $90\n",
        "  .bank 3\n  .org $E000\n  .db $03\nLoop:\n  jmp Loop\nNmi:\n  rti\n",
        "  .org $FFFA\n  .dw Nmi, Reset, Nmi\n",
    );
    let r = c.call(
        "assemble",
        json!({"input":"src.asm","sources":{"src.asm":source},"output":"roms/game.nes"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    c.write(
        "game.disasm.json",
        r#"{"rom": "roms/game.nes",
            "options": {"split_banks": true, "mapper4_fixed_prg_16k": true}}"#,
    );
    let r = c.call(
        "export_disassembly",
        json!({"annotations":"game.disasm.json"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let report = &r["structuredContent"];
    let names = |values: &Value, key: Option<&str>| -> Vec<String> {
        values
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                let path = key.map_or(v, |key| &v[key]).as_str().unwrap();
                std::path::Path::new(path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };
    let files = names(&report["files"], None);
    assert_eq!(
        files,
        [
            "game.asm",
            "game_bank00.asm",
            "game_bank01.asm",
            "game_bank02_03.asm"
        ]
    );
    assert_eq!(names(&report["artifacts"], Some("path")), files);
    let main = std::fs::read_to_string(c.root.join("roms/game.asm"))
        .unwrap()
        .replace("\r\n", "\n");
    assert!(
        main.contains("\t.catbank $02\n\t.include \"roms/game_bank02_03.asm\"\n"),
        "{main}"
    );
    let pair = std::fs::read_to_string(c.root.join("roms/game_bank02_03.asm"))
        .unwrap()
        .replace("\r\n", "\n");
    assert!(
        pair.contains("\tlda $0390,x\n; Bank 3 starts at $E000, inside the instruction above\n"),
        "{pair}"
    );
    let r = c.call(
        "assemble",
        json!({"input":"roms/game.asm","output":"rebuilt.nes"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        std::fs::read(c.root.join("rebuilt.nes")).unwrap(),
        std::fs::read(c.root.join("roms/game.nes")).unwrap()
    );
}

/// Builds `old/loose.nes`: 16 KiB NROM whose bytes at $E006-$E00A no code
/// reaches.
fn loose_rom(c: &mut Client) {
    let source = concat!(
        "  .inesprg 1\n  .ineschr 0\n  .bank 1\n  .org $E000\n",
        "Reset:\n  lda #0\n  sta $2000\nLoop:\n  jmp Loop\n  .db 1, 2, 3, 4, 5\nNmi:\n  rti\n",
        "  .org $FFFA\n  .dw Nmi, Reset, Nmi\n",
    );
    let r = c.call(
        "assemble",
        json!({"input":"src/loose.asm","sources":{"src/loose.asm":source},"output":"old/loose.nes"}),
    );
    assert_eq!(r["isError"], false, "{r}");
}

#[test]
fn annotate_writes_canonical_files_and_logs_evidence() {
    let mut c = Client::start();
    loose_rom(&mut c);
    let original = r#"{"rom": "old/loose.nes", "labels": [{"address": "$E005", "name": "Loop"}]}"#;
    c.write("loose.disasm.json", original);
    let digest = nesasm_core::disasm::sha256_hex(original.as_bytes());
    let table = json!([{"op": "upsert", "kind": "regions",
                        "item": {"start": "$E008", "length": 5, "comment": "unused"}}]);
    // Changing the file needs the digest of the file as read, which the
    // report gives.
    let r = c.call(
        "annotate",
        json!({"annotations":"loose.disasm.json","operations":table}),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(Client::messages(&r).contains("Give expected_sha256"), "{r}");
    assert_eq!(r["structuredContent"]["sha256_before"], digest.as_str());
    // A dry run reports the change and writes nothing.
    let r = c.call(
        "annotate",
        json!({"annotations":"loose.disasm.json","operations":table,"dry_run":true}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let report = &r["structuredContent"];
    assert_eq!(report["written"], false);
    assert_eq!(report["reformatted"], true);
    assert_eq!(report["verification"]["matched"], true);
    assert_eq!(
        report["annotations"]["regions"],
        json!([{"bank": 1, "start": "$E008", "length": 5, "comment": "unused"}])
    );
    assert_eq!(report["issues_resolved"]["total"], 1, "{r}");
    assert_eq!(
        report["issues_resolved"]["items"][0]["kind"],
        "unknown_range"
    );
    assert_eq!(report["issues_introduced"]["total"], 0);
    // The five bytes are data now (with the vectors; unknown bytes include
    // the padding).
    let bytes = |summary: &str, field: &str| report[summary]["banks"][1][field].as_u64().unwrap();
    let data = bytes("summary_before", "data_bytes");
    assert_eq!(bytes("summary_after", "data_bytes"), data + 5);
    let unknown = bytes("summary_before", "unknown_bytes");
    assert_eq!(bytes("summary_after", "unknown_bytes"), unknown - 5);
    assert_eq!(report["log_entry"]["sha256_before"], digest.as_str());
    assert_eq!(
        std::fs::read_to_string(c.root.join("loose.disasm.json")).unwrap(),
        original
    );
    assert!(!c.root.join("loose.disasm.log.jsonl").exists());
    // With the digest, the file is written in canonical form and the
    // operations with their evidence are logged.
    let evidence = "the bytes after JMP Loop are never read";
    let mut ops = table.clone();
    ops[0]["evidence"] = json!(evidence);
    let r = c.call(
        "annotate",
        json!({"annotations":"loose.disasm.json","operations":ops,"expected_sha256":digest}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let report = &r["structuredContent"];
    assert_eq!(report["written"], true);
    let written = std::fs::read_to_string(c.root.join("loose.disasm.json")).unwrap();
    assert_eq!(
        written,
        concat!(
            "{\n",
            "  \"rom\": \"old/loose.nes\",\n",
            "  \"format_version\": 2,\n",
            "  \"labels\": [\n",
            "    {\"bank\":1,\"address\":\"$E005\",\"name\":\"Loop\"}\n",
            "  ],\n",
            "  \"regions\": [\n",
            "    {\"bank\":1,\"start\":\"$E008\",\"length\":5,\"comment\":\"unused\",\"evidence\":[\"the bytes after JMP Loop are never read\"]}\n",
            "  ]\n",
            "}\n",
        )
    );
    let new_digest = nesasm_core::disasm::sha256_hex(written.as_bytes());
    assert_eq!(report["sha256"], new_digest.as_str());
    assert!(report.get("annotations").is_none(), "{r}");
    let log = std::fs::read_to_string(c.root.join("loose.disasm.log.jsonl")).unwrap();
    let entries: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["operations"][0]["evidence"], evidence);
    assert_eq!(entries[0]["sha256_after"], new_digest.as_str());
    // Totals of the PRG banks.
    let total = |when: &str, field: &str| entries[0][when][field].as_u64().unwrap();
    assert_eq!(
        total("before", "unknown_bytes"),
        total("after", "unknown_bytes") + 5
    );
    assert_eq!(
        total("after", "data_bytes"),
        total("before", "data_bytes") + 5
    );
    assert_eq!(
        (total("before", "issues"), total("after", "issues")),
        (1, 0)
    );
    let text = Client::text(&r);
    for line in [
        &format!(
            "Before: 4 instructions, code 9, data {data}, unknown {} bytes, 1 issues",
            total("before", "unknown_bytes")
        ),
        &format!(
            "After: 4 instructions, code 9, data {}, unknown {} bytes, 0 issues",
            data + 5,
            total("after", "unknown_bytes")
        ),
        "Resolved issues: 1",
    ] {
        assert!(
            text.lines().any(|l| l == line),
            "missing {line:?} in\n{text}"
        );
    }
    // The old digest no longer matches: nothing is written.
    let r = c.call(
        "annotate",
        json!({"annotations":"loose.disasm.json","operations":[],"expected_sha256":digest}),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(
        Client::messages(&r).contains("changed since it was read"),
        "{r}"
    );
    assert_eq!(r["structuredContent"]["sha256_before"], new_digest.as_str());
    // An unchanged canonical file is not reformatted.
    let r = c.call(
        "annotate",
        json!({"annotations":"loose.disasm.json","operations":[],"expected_sha256":new_digest}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["reformatted"], false);
    assert_eq!(r["structuredContent"]["sha256"], new_digest.as_str());
}

#[test]
fn annotate_writes_nothing_that_fails() {
    let mut c = Client::start();
    loose_rom(&mut c);
    // A changed operand that changes the bytes fails the round trip.
    let operand = json!([{"op": "upsert", "kind": "operands",
                          "item": {"address": "$E000", "expr": "#$01"}}]);
    let create = json!({"rom": "old/loose.nes"});
    let r = c.call(
        "annotate",
        json!({"annotations":"new.disasm.json","create":create,"operations":operand}),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(Client::messages(&r).contains("would not reassemble"), "{r}");
    assert_eq!(r["structuredContent"]["verification"]["matched"], false);
    assert!(!c.root.join("new.disasm.json").exists());
    // Unless the round trip is not required.
    let r = c.call(
        "annotate",
        json!({"annotations":"new.disasm.json","create":create,"operations":operand,
               "require_match":false}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["written"], true);
    assert!(
        r["structuredContent"].get("summary_before").is_none(),
        "{r}"
    );
    assert!(c.root.join("new.disasm.log.jsonl").exists());
    for (arguments, message) in [
        (
            json!({"annotations":"new.disasm.json","create":create,"operations":[]}),
            "exists",
        ),
        (
            json!({"annotations":"new.json","create":create,"operations":[]}),
            "must end with .disasm.json",
        ),
        (
            json!({"annotations":".hidden/x.disasm.json","create":create,"operations":[]}),
            "hidden",
        ),
        (
            json!({"annotations":"x.disasm.json","create":{"rom":"../outside.nes"},"operations":[]}),
            "outside.nes",
        ),
        (
            json!({"inline":{"rom":"old/loose.nes"},"create":create,"operations":[]}),
            "Give annotations",
        ),
        (
            json!({"inline":{"rom":"old/loose.nes"},"expected_sha256":"00","operations":[]}),
            "expected_sha256 applies only to an existing annotation file",
        ),
        (
            json!({"inline":{"rom":"old/loose.nes"},
                   "operations":[{"op":"remove","kind":"labels","key":{"name":"Nope"}}]}),
            "operations[0] (remove labels): no entry matches the key",
        ),
        (
            json!({"inline":{"rom":"old/loose.nes"},
                   "operations":[{"op":"upsert","kind":"entry_points",
                                  "item":{"address":"$E008","no_return":true}}]}),
            "give evidence",
        ),
    ] {
        let r = c.call("annotate", arguments.clone());
        assert_eq!(r["isError"], true, "{arguments}: {r}");
        assert!(Client::messages(&r).contains(message), "{arguments}: {r}");
    }
    assert!(!c.root.join("x.disasm.json").exists());
    assert!(!c.root.join(".hidden").exists());
}

#[test]
fn annotate_inline_returns_the_annotations_and_suggestions_apply() {
    let mut c = Client::start();
    let source = concat!(
        "  .inesprg 1\n  .ineschr 0\n  .bank 1\n  .org $E000\n",
        "Reset:\n  ldx $0300\n  lda Hi,x\n  pha\n  lda Lo,x\n  pha\n  rts\n",
        "A1:\n  rts\nA2:\n  rts\nLo:\n  .db LOW(A1-1), LOW(A2-1)\nHi:\n  .db HIGH(A1-1), HIGH(A2-1)\n",
        "Nmi:\n  rti\n  .org $FFFA\n  .dw Nmi, Reset, Nmi\n",
    );
    let r = c.call(
        "assemble",
        json!({"input":"rts.asm","sources":{"rts.asm":source},"output":"rts.nes"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let r = c.call(
        "disassemble",
        json!({"inline":{"rom":"rts.nes"},"issues":{"kind":"indirect_jump"}}),
    );
    let issue = &r["structuredContent"]["issues"]["items"][0];
    let suggestion = &issue["suggestions"][0];
    assert_eq!(suggestion["needs"], json!(["length"]), "{r}");
    assert!(
        Client::text(&r).contains("and a high_bytes region [1 suggestions]"),
        "{r}"
    );
    let mut ops = suggestion["operations"].clone();
    for op in ops.as_array_mut().unwrap() {
        op["item"]["length"] = json!(2);
        op["evidence"] = json!("Reset reads Lo,x and Hi,x for X 0 and 1");
    }
    let r = c.call(
        "annotate",
        json!({"inline":{"rom":"rts.nes"},"operations":ops}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let report = &r["structuredContent"];
    assert_eq!(report["written"], false);
    assert!(report.get("sha256_before").is_none(), "{r}");
    // The jump is still reported, saying that the region describes it.
    assert_eq!(report["issues_changed"]["total"], 1, "{r}");
    let changed = &report["issues_changed"]["items"][0];
    assert_eq!(changed["id"], "indirect_jump:1:$E00B");
    assert!(
        changed["previous_message"]
            .as_str()
            .unwrap()
            .contains("describe them with a low_bytes region"),
        "{r}"
    );
    assert!(
        Client::text(&r).lines().any(|l| l == "Changed issues: 1"),
        "{r}"
    );
    assert_eq!(report["annotations"]["regions"][0]["type"], "low_bytes");
    assert_eq!(
        report["log_entry"]["operations"][0]["evidence"],
        "Reset reads Lo,x and Hi,x for X 0 and 1"
    );
    assert!(report["log_entry"]["annotations"].is_null());
    // The jump's targets are code now, and the issue no longer suggests.
    let r = c.call(
        "disassemble",
        json!({"inline":report["annotations"],"issues":{"kind":"indirect_jump"}}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let issue = &r["structuredContent"]["issues"]["items"][0];
    assert!(issue.get("suggestions").is_none(), "{r}");
}

#[test]
fn annotate_compares_inline_annotations_with_themselves_before() {
    let mut c = Client::start();
    loose_rom(&mut c);
    let r = c.call(
        "annotate",
        json!({"inline":{"rom":"old/loose.nes"},"operations":[]}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let report = &r["structuredContent"];
    assert_eq!(report["summary_before"], report["summary_after"]);
    assert_eq!(report["issues_resolved"]["total"], 0);
    assert_eq!(report["issues_introduced"]["total"], 0);
}

#[test]
fn disassemble_compares_with_baseline_annotations() {
    let mut c = Client::start();
    loose_rom(&mut c);
    c.write("before.disasm.json", r#"{"rom": "old/loose.nes"}"#);
    let current = json!({"rom": "old/loose.nes",
                         "regions": [{"start": "$E008", "length": 5}]});
    let r = c.call(
        "disassemble",
        json!({"inline":current,"baseline":{"annotations":"before.disasm.json"}}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let baseline = &r["structuredContent"]["baseline"];
    assert_eq!(baseline["issues_resolved"]["total"], 1, "{r}");
    assert_eq!(
        baseline["issues_resolved"]["items"][0]["id"],
        "unknown_range:1:$E008"
    );
    assert_eq!(baseline["issues_introduced"]["total"], 0);
    assert_eq!(baseline["issues_changed"]["total"], 0);
    assert_eq!(baseline["summary"]["banks"][1]["org"], "$E000");
    let text = Client::text(&r);
    for line in [
        "Compared with the baseline: 1 resolved, 0 introduced, 0 changed issues",
        "Resolved issues: 1",
    ] {
        assert!(
            text.lines().any(|l| l == line),
            "missing {line:?} in\n{text}"
        );
    }
    // A baseline that cannot be analysed fails the request.
    let r = c.call(
        "disassemble",
        json!({"inline":current,"baseline":{"annotations":"missing.disasm.json"}}),
    );
    assert_eq!(r["isError"], true, "{r}");
    let message = r["structuredContent"]["diagnostics"][0]["message"]
        .as_str()
        .unwrap();
    assert!(message.starts_with("Baseline: "), "{r}");
    assert!(r["structuredContent"].get("baseline").is_none(), "{r}");
}

#[test]
fn disassembly_reports_name_their_inputs() {
    let mut c = Client::start();
    loose_rom(&mut c);
    let annotations = r#"{"rom": "old/loose.nes"}"#;
    c.write("loose.disasm.json", annotations);
    let rom = std::fs::read(c.root.join("old/loose.nes")).unwrap();
    let rom_sha = nesasm_core::disasm::sha256_hex(&rom);
    let r = c.call("disassemble", json!({"annotations":"loose.disasm.json"}));
    assert_eq!(r["isError"], false, "{r}");
    let inputs = &r["structuredContent"]["inputs"];
    assert_eq!(inputs["rom_sha256"], rom_sha.as_str());
    assert_eq!(
        inputs["annotations_sha256"],
        nesasm_core::disasm::sha256_hex(annotations.as_bytes()).as_str()
    );
    assert_eq!(inputs["server_version"], env!("CARGO_PKG_VERSION"));
    assert!(inputs.get("base_sha256").is_none(), "{r}");
    let line = format!(
        "Inputs: ROM sha256 {rom_sha}, annotations sha256 {}, nesasm-mcp {}",
        nesasm_core::disasm::sha256_hex(annotations.as_bytes()),
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        Client::text(&r).lines().any(|l| l == line),
        "{}",
        Client::text(&r)
    );
    // Inline annotations: the digest of their canonical text.
    let inline: nesasm_core::disasm::DisasmConfig =
        serde_json::from_str(r#"{"rom": "old/loose.nes", "options": {"uppercase": true}}"#)
            .unwrap();
    let r = c.call(
        "export_disassembly",
        json!({"inline":{"rom":"old/loose.nes","options":{"uppercase":true}},"output":"out/loose.asm"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        r["structuredContent"]["inputs"]["annotations_sha256"],
        nesasm_core::disasm::sha256_hex(nesasm_core::disasm::canonical_json(&inline).as_bytes())
            .as_str()
    );
    // A baseline for another ROM is compared with a warning.
    let mut other = rom.clone();
    let last = other.len() - 7;
    other[last] ^= 0xFF;
    c.write("old/other.nes", &other);
    let r = c.call(
        "disassemble",
        json!({"annotations":"loose.disasm.json","baseline":{"inline":{"rom":"old/other.nes"}}}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let report = &r["structuredContent"];
    assert_eq!(report["diagnostics"][0]["severity"], "warning");
    assert!(
        Client::messages(&r).contains("another ROM or base ROM"),
        "{r}"
    );
    assert_eq!(
        report["baseline"]["inputs"]["rom_sha256"],
        nesasm_core::disasm::sha256_hex(&other).as_str()
    );
    // The same ROM under another name is no warning.
    c.write("old/copy.nes", &rom);
    let r = c.call(
        "disassemble",
        json!({"annotations":"loose.disasm.json","baseline":{"inline":{"rom":"old/copy.nes"}}}),
    );
    assert_eq!(r["structuredContent"]["diagnostics"], json!([]), "{r}");
}

#[test]
fn annotate_errors_name_entries_of_the_changed_annotations() {
    let mut c = Client::start();
    loose_rom(&mut c);
    // Canonical order puts the invalid label first.
    let inline = json!({"rom": "old/loose.nes", "labels": [
        {"address": "$E005", "name": "Fine"}, {"address": "$E000", "name": "9lives"}]});
    let r = c.call("annotate", json!({"inline":inline,"operations":[]}));
    assert_eq!(r["isError"], true, "{r}");
    assert!(
        Client::messages(&r).contains(
            "In the changed annotations: labels[0] $E000 (9lives): Invalid name '9lives'"
        ),
        "{r}"
    );
    assert_eq!(
        r["structuredContent"]["annotations"]["labels"][0]["name"],
        "9lives"
    );
    // The digest of inline annotations is that of what annotate writes.
    let inline = json!({"rom": "old/loose.nes", "labels": [{"address": "$E005", "name": "Fine"}]});
    let d = c.call("disassemble", json!({"inline":inline}));
    let a = c.call("annotate", json!({"inline":inline,"operations":[]}));
    assert_eq!(a["isError"], false, "{a}");
    assert_eq!(
        d["structuredContent"]["inputs"]["annotations_sha256"],
        a["structuredContent"]["sha256"]
    );
}

#[test]
fn inline_digests_tell_the_order_of_names_for_one_place() {
    let mut c = Client::start();
    loose_rom(&mut c);
    let digest = |c: &mut Client, first: &str, second: &str| {
        let inline = json!({"rom": "old/loose.nes", "labels": [
            {"address": "$E000", "name": first}, {"address": "$E000", "name": second}]});
        let r = c.call("disassemble", json!({"inline":inline}));
        assert_eq!(r["isError"], false, "{r}");
        r["structuredContent"]["inputs"]["annotations_sha256"].clone()
    };
    let one = digest(&mut c, "Zulu", "Alpha");
    let other = digest(&mut c, "Alpha", "Zulu");
    assert_ne!(one, other);
    // And of symbols whose ranges overlap.
    let higher = json!({"name": "Higher", "value": "$30", "size": 32});
    let lower = json!({"name": "Lower", "value": "$20", "size": 64});
    let mut digests = Vec::new();
    for symbols in [json!([higher, lower]), json!([lower, higher])] {
        let r = c.call(
            "disassemble",
            json!({"inline":{"rom":"old/loose.nes","symbols":symbols}}),
        );
        digests.push(r["structuredContent"]["inputs"]["annotations_sha256"].clone());
    }
    assert_ne!(digests[0], digests[1]);
}

#[test]
fn hypotheses_are_kept_but_applied_only_on_request() {
    let mut c = Client::start();
    loose_rom(&mut c);
    c.write(
        "loose.disasm.json",
        r#"{"rom": "old/loose.nes", "format_version": 2, "labels": [
            {"address": "$E005", "name": "Spin", "status": "hypothesis", "evidence": ["jumps to itself"]}]}"#,
    );
    let view = json!({"from": "$E000", "to": "$E00F"});
    let r = c.call(
        "disassemble",
        json!({"annotations":"loose.disasm.json","view":view}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let report = &r["structuredContent"];
    assert_eq!(report["hypotheses"], json!({"count": 1, "applied": false}));
    assert!(!report["view"].to_string().contains("Spin"), "{r}");
    assert!(
        Client::text(&r)
            .lines()
            .any(|l| l == "Hypotheses: 1 entries, not applied (hypotheses: true applies them)"),
        "{r}"
    );
    let r = c.call(
        "disassemble",
        json!({"annotations":"loose.disasm.json","view":view,"hypotheses":true}),
    );
    assert_eq!(r["structuredContent"]["hypotheses"]["applied"], true);
    assert!(
        r["structuredContent"]["view"].to_string().contains("Spin:"),
        "{r}"
    );
    // Never in the source.
    let r = c.call(
        "export_disassembly",
        json!({"annotations":"loose.disasm.json","output":"out/loose.asm"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let source = std::fs::read_to_string(c.root.join("out/loose.asm")).unwrap();
    assert!(!source.contains("Spin"), "{source}");
    // annotate checks hypotheses it does not apply.
    let r = c.call(
        "annotate",
        json!({"inline":{"rom":"old/loose.nes"},"operations":[{"op":"upsert","kind":"labels",
               "item":{"address":"$E000","name":"9x","status":"hypothesis"},"evidence":"a guess"}]}),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(
        Client::messages(&r).contains(
            "In the changed annotations (with the hypotheses applied): labels[0] $E000 (9x): Invalid name '9x'"
        ),
        "{r}"
    );
}

#[test]
fn annotate_checks_the_annotations_without_the_hypotheses_too() {
    let mut c = Client::start();
    loose_rom(&mut c);
    // Bank 0 holds $8000 only if the hypothetical layout is applied.
    let inline = json!({"rom": "old/loose.nes", "format_version": 2,
        "banks": [{"bank": 0, "org": "$8000", "status": "hypothesis", "evidence": ["a guess"]}],
        "labels": [{"bank": 0, "address": "$8000", "name": "Start8000"}]});
    let r = c.call(
        "annotate",
        json!({"inline":inline,"operations":[],"hypotheses":true}),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(
        Client::messages(&r).contains(
            "In the changed annotations (without the hypotheses): labels[0] $8000 (Start8000): Bank 0 ($C000-$DFFF) does not hold $8000"
        ),
        "{r}"
    );
}

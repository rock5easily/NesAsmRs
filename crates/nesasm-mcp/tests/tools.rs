mod common;

use common::Client;
use serde_json::{Value, json};

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

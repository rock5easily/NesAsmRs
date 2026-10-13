mod common;

use common::Temp;
use nesasm_core::disasm::{self, Address, DisasmConfig, Disassembly, IssueKind, XrefKind};
use nesasm_core::{AssembleOptions, AssembleRequest, AssembleResult};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

const PROJECT_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

static NEVER: AtomicBool = AtomicBool::new(false);

/// The ROM file an assembly would write: header and payload.
fn rom_file(result: &AssembleResult) -> Vec<u8> {
    let mut file = result.header.clone();
    file.extend(&result.binary);
    file
}

fn config(json: &str) -> DisasmConfig {
    disasm::parse_config(json).unwrap()
}

/// Disassembles `rom` and checks that the source reassembles to it.
fn round_trip(rom: &[u8], config: &DisasmConfig) -> Disassembly {
    let t = Temp::new();
    let d = disasm::disassemble(rom, None, config, Path::new("out.asm"), &NEVER)
        .unwrap_or_else(|e| panic!("{e:?}"));
    let v = d.verify(&t.0, Some(&t.0), &NEVER);
    assert!(v.matched, "{v:#?}\n{}", d.text());
    d
}

/// Assembles `text` and returns the ROM file.
fn assemble(text: &str) -> Vec<u8> {
    let t = Temp::new();
    let r = t.run(text);
    assert!(r.success, "{:?}", r.diagnostics);
    rom_file(&r)
}

fn fixtures(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            fixtures(&path, found);
        } else if path.extension().is_some_and(|e| e == "asm") {
            found.push(path);
        }
    }
}

#[test]
fn every_compat_fixture_round_trips() {
    let mut paths = Vec::new();
    fixtures(
        &Path::new(PROJECT_ROOT).join("tools/compat/fixtures"),
        &mut paths,
    );
    let mut checked = 0;
    for path in paths {
        let request = AssembleRequest {
            input: path.file_name().unwrap().into(),
            working_directory: path.parent().unwrap().to_owned(),
            options: AssembleOptions::default(),
            ..Default::default()
        };
        let result = nesasm_core::assemble(&request);
        if !result.success || result.binary.is_empty() {
            continue;
        }
        let rom = rom_file(&result);
        let d = disasm::disassemble(
            &rom,
            None,
            &DisasmConfig::default(),
            Path::new("out.asm"),
            &NEVER,
        )
        .unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
        let t = Temp::new();
        let v = d.verify(&t.0, Some(&t.0), &NEVER);
        assert!(v.matched, "{}: {v:#?}", path.display());
        checked += 1;
    }
    assert!(checked >= 10, "only {checked} fixtures assembled");
}

/// A small deterministic generator, so random ROMs need no dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u8 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u8
    }
}

#[test]
fn random_roms_round_trip() {
    let mut rng = Lcg(0x5eed);
    for (prg, chr) in [(1u8, 0u8), (1, 1), (2, 1), (8, 0)] {
        let mut rom = vec![
            b'N', b'E', b'S', 0x1a, prg, chr, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let size = usize::from(prg) * 16384 + usize::from(chr) * 8192;
        rom.extend((0..size).map(|_| rng.next()));
        round_trip(&rom, &DisasmConfig::default());
    }
    // Raw banks and fragments, with code everywhere.
    let bin: Vec<u8> = (0..16384).map(|_| rng.next()).collect();
    round_trip(&bin, &DisasmConfig::default());
    for load in [0x8000u32, 0xc123, 0x0300, 0xfff0] {
        let len = (0x10000 - load as usize).min(5000);
        let fragment: Vec<u8> = (0..len).map(|_| rng.next()).collect();
        let c = config(&format!(
            r#"{{"load_address": {load}, "regions": [{{"start": {load}, "length": {len}, "type": "code"}}]}}"#
        ));
        round_trip(&fragment, &c);
    }
}

#[test]
fn traces_code_from_the_vectors() {
    let rom = assemble(concat!(
        "  .inesprg 1\n  .ineschr 0\n  .inesmap 0\n  .inesmir 1\n",
        "  .bank 0\n  .org $C000\n",
        "Start:\n  sei\n  ldx #$FF\n  txs\n  jsr Init\nLoop:\n  lda <$10\n  sta $0300,x\n  bne Loop\n  jmp Loop\n",
        "Init:\n  lda Table,x\n  sta $2000\n  rts\n",
        "Table:\n  .db 1, 2, 3, 4\n",
        "Nmi:\n  rti\n",
        "  .bank 1\n  .org $FFFA\n  .dw Nmi, Start, Nmi\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    let text = d.text();
    for line in [
        "\t.inesprg 1",
        "\t.inesmir 1",
        "PPUCTRL         = $2000",
        "RESET:",
        "\tsei",
        "\tldx #$FF",
        "\tjsr sub_C011",
        "L_C007:",
        "\tlda <$10",
        "\tsta $0300,x",
        "\tbne L_C007",
        "sub_C011:",
        "\tlda D_C018,x",
        "\tsta PPUCTRL",
        "D_C018:",
        "\t.db $01, $02, $03, $04",
        "NMI:",
        "\trti",
        "\t.dw NMI, RESET, NMI",
    ] {
        assert!(
            text.lines().any(|l| l == line),
            "missing {line:?} in\n{text}"
        );
    }
    assert!(text.starts_with("; Generated by nesasm disassembler; checksum "));
    assert!(disasm::is_unedited_source(text));
    assert!(!disasm::is_unedited_source(&text.replace("sei", "cli")));
}

/// Lines of `text` that `d`'s source must contain, in order.
fn assert_lines(d: &Disassembly, expected: &[&str]) {
    let text = d.text();
    let mut lines = text.lines();
    for line in expected {
        assert!(
            lines.any(|l| l == *line),
            "missing {line:?} (in order) in\n{text}"
        );
    }
}

fn issues(d: &Disassembly, kind: IssueKind) -> Vec<String> {
    d.issues()
        .iter()
        .filter(|i| i.kind == kind)
        .map(|i| format!("{} {}", i.address, i.message))
        .collect()
}

/// A 16 KiB NROM ROM: `code` in bank 1 at $E000, vectors to Nmi/Reset.
fn nrom(code: &str) -> Vec<u8> {
    assemble(&format!(
        "  .inesprg 1\n  .ineschr 0\n  .bank 1\n  .org $E000\n{code}\n  .org $FFFA\n  .dw Nmi, Reset, Nmi\n"
    ))
}

#[test]
fn annotations_name_and_shape_the_source() {
    let rom = assemble(concat!(
        "  .inesprg 1\n  .bank 0\n  .org $C000\n",
        "Reset:\n  jsr Print\n  .db $AA, $BB\n  ldx $0301\n  lda Lo,x\n  sta <$10\n",
        "  lda Hi,x\n  sta <$11\n  jmp [$0010]\n",
        "Print:\n  rts\nHalt:\n  jmp Halt\n",
        "Lo:\n  .db LOW(Halt), LOW(Print)\nHi:\n  .db HIGH(Halt), HIGH(Print)\n",
        "Ptrs:\n  .dw Halt-1\nMsg:\n  .db \"HI!\", 0\nNmi:\n  rti\n",
        "  .bank 1\n  .org $FFFA\n  .dw Nmi, Reset, Nmi\n",
    ));
    let c = config(
        r#"{
        "labels": [{"address": "$C015", "name": "Print", "comment": "Prints the text after the JSR"}],
        "entry_points": [{"address": "$C015", "inline_bytes": 2}],
        "symbols": [
            {"name": "buffer", "value": "$0300", "size": 4, "comment": "work area"},
            {"name": "ptr", "value": 16}
        ],
        "regions": [
            {"start": "$C019", "length": 2, "type": "low_bytes", "pair": "$C01B", "code_targets": true},
            {"start": "$C01B", "length": 2, "type": "high_bytes"},
            {"start": "$C01D", "length": 2, "type": "pointers", "adjust": -1, "code_targets": true},
            {"start": "$C01F", "end": "$C022", "type": "text", "comment": "Message"}
        ],
        "comments": [
            {"address": "$C005", "text": "read the count"},
            {"address": "$C000", "text": "Entry\nafter reset", "position": "above"}
        ],
        "operands": [{"address": "0xC010", "expr": "<ptr+1"}]
    }"#,
    );
    let d = round_trip(&rom, &c);
    assert_lines(
        &d,
        &[
            "buffer          = $0300 ; work area",
            "ptr             = $10",
            "; Entry",
            "; after reset",
            "RESET:",
            "\tjsr Print",
            "\t.db $AA, $BB",
            "\tldx buffer+1 ; read the count",
            "\tlda D_C019,x",
            "\tsta <ptr",
            "\tlda D_C01B,x",
            "\tsta <ptr+1",
            "\tjmp [ptr]",
            "; Prints the text after the JSR",
            "Print:",
            "\trts",
            "L_C016:",
            "\tjmp L_C016",
            "D_C019:",
            "\t.db LOW(L_C016), LOW(Print)",
            "D_C01B:",
            "\t.db HIGH(L_C016), HIGH(Print)",
            "\t.dw L_C016-1",
            "; Message",
            "\t.db \"HI!\", $00",
            "NMI:",
            "\trti",
        ],
    );
    assert_eq!(
        issues(&d, IssueKind::IndirectJump),
        [
            "$C012 Indirect JMP [$0010] at $C012; the address comes from the table at $C019 indexed by X, which a region describes"
        ]
    );
}

#[test]
fn zero_page_and_absolute_operands_keep_their_encoding() {
    let rom = nrom(
        "Reset:\n  lda $0012\n  lda <$12\n  sta $0012,y\n  ldx <$12,y\n  lda [$12],y\n  sta [$12,x]\nNmi:\n  rti",
    );
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_lines(
        &d,
        &[
            "\tlda $0012",
            "\tlda <$12",
            "\tsta $0012,y",
            "\tldx <$12,y",
            "\tlda [$12],y",
            "\tsta [$12,x]",
        ],
    );
    let c = config(
        r#"{"symbols": [{"name": "zvar", "value": "$12"}], "options": {"uppercase": true}}"#,
    );
    let d = round_trip(&rom, &c);
    assert_lines(
        &d,
        &[
            "\tLDA zvar",
            "\tLDA <zvar",
            "\tSTA zvar,Y",
            "\tLDX <zvar,Y",
            "\tLDA [zvar],Y",
            "\tSTA [zvar,X]",
        ],
    );
}

#[test]
fn entries_inside_an_instruction_are_relative_to_it() {
    // The BIT absolute opcode ($2C) skips the next instruction.
    let rom = nrom(concat!(
        "First:\n  lda #1\n  .db $2C\nSecond:\n  lda #2\n  rts\n",
        "Reset:\n  jsr First\n  jsr Second\n  jmp Reset\nNmi:\n  rti",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_lines(
        &d,
        &[
            "sub_E000:",
            "L_E002:",
            "\tbit $02A9",
            "\tjsr sub_E000",
            "\tjsr L_E002+1",
        ],
    );
    assert_eq!(issues(&d, IssueKind::Overlap).len(), 1);
    let d = round_trip(
        &rom,
        &config(r#"{"labels": [{"address": "$E003", "name": "Second"}]}"#),
    );
    assert_lines(
        &d,
        &[
            "L_E002:",
            "\tbit $02A9",
            "Second = L_E002+1",
            "\tjsr Second",
        ],
    );
}

#[test]
fn references_choose_between_banks_at_one_address() {
    let t = Temp::new();
    let r = t.run_with(
        concat!(
            "  .bank 0\n  .org $8000\n  rts\n",
            "  .bank 1\n  .org $8000\n  lda #1\n  rts\n",
            "  .bank 3\n  .org $E000\nReset:\n  jsr $8000\n  jmp Reset\n",
            "  .org $FFFA\n  .dw Reset, Reset, Reset\n",
        ),
        AssembleOptions {
            raw: true,
            ..AssembleOptions::default()
        },
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(r.binary.len(), 4 * 8192);
    let banks = r#""banks": [{"bank": 1, "org": "$8000"}]"#;
    let d = round_trip(&r.binary, &config(&format!("{{{banks}}}")));
    assert_lines(
        &d,
        &[
            "; Assemble with -raw (no iNES header).",
            "\t.bank 3",
            "\tjsr $8000",
        ],
    );
    assert_eq!(
        issues(&d, IssueKind::UnresolvedBank),
        ["$E000 $8000 is held by banks 0, 1; add a references entry with target_bank"]
    );
    let c = config(&format!(
        r#"{{{banks}, "references": [{{"address": "$E000", "target_bank": 1}}]}}"#
    ));
    let d = round_trip(&r.binary, &c);
    assert_lines(
        &d,
        &[
            "\t.bank 1",
            "\t.org $8000",
            "sub_01_8000:",
            "\tlda #$01",
            "\tjsr sub_01_8000",
        ],
    );
    assert!(issues(&d, IssueKind::UnresolvedBank).is_empty());
}

#[test]
fn fragments_are_placed_at_their_load_address() {
    let code = [0xa9, 0x00, 0x8d, 0x00, 0x20, 0x60];
    let c = config(
        r#"{"load_address": "$0300", "entry_points": [{"address": "$0300", "name": "Start"}]}"#,
    );
    let d = round_trip(&code, &c);
    assert_lines(
        &d,
        &[
            "PPUCTRL         = $2000",
            "\t.bank 0",
            "\t.org $0300",
            "Start:",
            "\tlda #$00",
            "\tsta PPUCTRL",
            "\trts",
        ],
    );
    let e = disasm::disassemble(
        &code,
        None,
        &DisasmConfig::default(),
        Path::new("out.asm"),
        &NEVER,
    )
    .err()
    .unwrap();
    assert_eq!(e[0].code.as_str(), "E_DISASM");
    assert_eq!(
        e[0].message,
        "The file has 6 bytes, not a whole number of 8 KiB banks; give load_address for a fragment"
    );
}

#[test]
fn chr_banks_are_included_or_written_as_data() {
    let mut rom = nrom("Reset:\nNmi:\n  rti");
    let chr: Vec<u8> = (0..8192u32).map(|i| (i / 3) as u8).collect();
    rom[5] = 1;
    rom.extend(&chr);
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_lines(
        &d,
        &[
            "\t.ineschr 1",
            "\t.bank 2",
            "\t.incbin \"out.chr\", 0, 8192",
        ],
    );
    let (path, data) = d.chr_file().unwrap();
    assert_eq!((path, data), (Path::new("out.chr"), chr.as_slice()));
    let d = round_trip(
        &rom,
        &config(r#"{"options": {"chr": "db", "data_per_line": 4}}"#),
    );
    assert_lines(
        &d,
        &["\t.bank 2", "\t.org $0000", "\t.db $00, $00, $00, $01"],
    );
    assert!(d.chr_file().is_none());
}

#[test]
fn unsupported_header_fields_are_reported_and_fail_verification() {
    let mut rom = nrom("Reset:\nNmi:\n  rti");
    rom[7] = 0x08;
    let d = disasm::disassemble(
        &rom,
        None,
        &DisasmConfig::default(),
        Path::new("out.asm"),
        &NEVER,
    )
    .unwrap();
    assert_eq!(
        issues(&d, IssueKind::Header),
        [
            "$0000 Header bytes 7 (low nibble) and 8-15 are not zero (NES 2.0 or extra fields); NESASM writes them as zero"
        ]
    );
    let t = Temp::new();
    let v = d.verify(&t.0, Some(&t.0), &NEVER);
    assert!(!v.matched);
    assert_eq!(v.mismatch_count, 1);
    let m = &v.mismatches[0];
    assert_eq!(
        (m.bank, m.address, m.expected.as_str(), m.actual.as_str()),
        (None, Address(7), "08", "00")
    );
}

#[test]
fn flow_problems_are_issues() {
    let rom = nrom(concat!(
        "Reset:\n  sta $8000\n  lda #0\n  beq Bad\n  rts\n",
        "Bad:\n  .db $02\nNmi:\n  rti\nLoose:\n  .db 1, 2, 3, 4, 5\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_eq!(
        issues(&d, IssueKind::MapperWrite),
        ["$E000 Write to ROM address $8000 at $E000: a mapper register (bank switching?)"]
    );
    assert_eq!(
        issues(&d, IssueKind::InvalidOpcode),
        [
            "$E008 $02 at $E008 (from $E005) is not an NMOS 6502 instruction; the flow may be wrong, or this is data"
        ]
    );
    assert_eq!(
        issues(&d, IssueKind::UnknownRange),
        [
            "$E008 $E008-$E008 (1 bytes) is not reached by code nor described by a region",
            "$E00A $E00A-$E00E (5 bytes) is not reached by code nor described by a region"
        ]
    );
    // The padding up to the vectors is not reported.
    assert_lines(
        &d,
        &[
            "\tsta $8000",
            "L_E008:",
            "\t.db $02",
            "NMI:",
            "\t.db $01, $02, $03, $04, $05",
            "\t.ds 8171",
        ],
    );
    let summary = d.summary();
    assert_eq!(summary.issues[&IssueKind::UnknownRange], 2);
    assert_eq!(summary.banks[1].code_bytes, 9);
}

#[test]
fn annotation_errors_name_the_entry() {
    let rom = nrom("Reset:\nNmi:\n  rti");
    for (json, message) in [
        (
            r#"{"labels": [{"address": "$E000", "name": "9lives"}]}"#,
            "labels[0] $E000 (9lives): Invalid name '9lives'",
        ),
        (
            r#"{"labels": [{"address": "$E000", "name": "ok"}, {"address": "$1234", "name": "a2"}]}"#,
            "labels[1] $1234 (a2): No PRG bank holds $1234",
        ),
        (
            r#"{"regions": [{"start": "$E000", "length": 1}, {"start": "$E001", "length": 1, "type": "high_bytes"}]}"#,
            "regions[1] $E001: no low_bytes region names this high_bytes table as its pair",
        ),
        (
            r#"{"comments": [{"address": "$1234", "text": "x"}]}"#,
            "comments[0] $1234: No PRG bank holds $1234",
        ),
        (
            r#"{"references": [{"address": "$E000", "target_bank": 9}]}"#,
            "references[0] $E000: bank 9 is not a PRG bank of the ROM",
        ),
        (
            r#"{"labels": [{"address": "$E000", "name": "x"}]}"#,
            "Name 'x' is reserved by the assembler",
        ),
        (
            r#"{"labels": [{"address": "$E000", "name": "a1"}], "symbols": [{"name": "a1", "value": 0}]}"#,
            "Name 'a1' is used by labels[0] $E000 (a1) and symbols[0] $0000 (a1)",
        ),
        (
            r#"{"labels": [{"address": "$1234", "name": "a1"}]}"#,
            "labels[0] $1234 (a1): No PRG bank holds $1234",
        ),
        (
            r#"{"labels": [{"bank": 0, "address": "$E000", "name": "a1"}]}"#,
            "labels[0] $E000 (a1): Bank 0 ($C000-$DFFF) does not hold $E000",
        ),
        (
            r#"{"regions": [{"start": "$E000", "length": 4}, {"start": "$E002", "length": 4}]}"#,
            "regions[0] and regions[1]: Regions at $E000 and $E002 in bank 1 overlap",
        ),
        (
            r#"{"regions": [{"start": "$E000", "length": 2, "type": "low_bytes"}]}"#,
            "regions[0] $E000: low_bytes needs pair (the high_bytes table)",
        ),
        (
            r#"{"regions": [{"start": "$FFFF", "length": 2}]}"#,
            "regions[0] $FFFF: the range runs past the end of bank 1 ($FFFF)",
        ),
        (
            r#"{"banks": [{"bank": 0, "org": "$9000"}]}"#,
            "banks[0]: Bank 0 org $9000 must be a multiple of $2000 up to $E000",
        ),
        (
            r#"{"load_address": "$8000"}"#,
            "load_address applies only to raw .bin files",
        ),
        (
            r#"{"operands": [{"address": "$E000", "expr": " "}]}"#,
            "operands[0] $E000: the expression must be one line without a comment",
        ),
    ] {
        let e = disasm::disassemble(&rom, None, &config(json), Path::new("out.asm"), &NEVER)
            .err()
            .unwrap_or_else(|| panic!("{json} should fail"));
        assert!(
            e.iter()
                .any(|d| d.code.as_str() == "E_DISASM" && d.message.contains(message)),
            "{json}: {:?}",
            e.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }
    for (json, message) in [
        (
            r#"{"labels": [{"address": "C000", "name": "a"}]}"#,
            "Invalid address 'C000'",
        ),
        (r#"{"label": []}"#, "unknown field `label`"),
    ] {
        let e = disasm::parse_config(json).unwrap_err();
        assert!(e.contains(message), "{json}: {e}");
    }
}

#[test]
fn queries_report_routines_ram_and_references() {
    let rom = nrom(concat!(
        "Reset:\n  jsr Read\n  jsr Read\n  sta $0300\n  jmp Reset\n",
        "Read:\n  lda $4016\n  inc <$20\n  rts\nNmi:\n  rti",
    ));
    let d = round_trip(
        &rom,
        &config(r#"{"symbols": [{"name": "count", "value": "$20"}]}"#),
    );
    let routines = d.routines();
    let read = routines.iter().find(|r| r.name == "sub_E00C").unwrap();
    assert_eq!((read.callers, read.size), (2, 6));
    assert_eq!(
        (read.reads.clone(), read.writes.clone()),
        (vec!["JOY1".to_owned()], vec!["count".to_owned()])
    );
    let reset = routines.iter().find(|r| r.name == "RESET").unwrap();
    assert_eq!(reset.calls, ["sub_E00C"]);
    assert_eq!(reset.writes, ["$0300"]);
    let ram = d.ram();
    let count = ram.iter().find(|r| r.address == Address(0x20)).unwrap();
    assert_eq!(
        (count.name.as_deref(), count.reads, count.writes),
        (Some("count"), 0, 1)
    );
    assert_eq!(count.routines, ["sub_E00C"]);
    let calls = d.xrefs("sub_E00C").unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        (calls[0].address, calls[0].line.as_str()),
        (Address(0xe000), "jsr sub_E00C")
    );
    assert_eq!(calls[0].routine.as_deref(), Some("RESET"));
    assert_eq!(d.xrefs("$0300").unwrap().len(), 1);
    assert_eq!(d.xrefs("count").unwrap()[0].line, "inc <count");
    assert!(d.xrefs("nothing").is_err());
    let view: Vec<&str> = d
        .view(Some(1), 0xe00c, 0xe011)
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(view, ["sub_E00C:", "\tlda JOY1", "\tinc <count", "\trts"]);
}

#[test]
fn addresses_accept_numbers_and_hex_strings() {
    for (text, value) in [
        ("49152", 49152),
        ("$C000", 0xc000),
        ("0xc000", 0xc000),
        ("$C0_00", 0xc000),
    ] {
        assert_eq!(Address::parse(text), Ok(Address(value)), "{text}");
    }
    for text in ["", "$", "C000", "-1", "+5", "$10000000000"] {
        assert!(Address::parse(text).is_err(), "{text}");
    }
    assert_eq!(serde_json::to_string(&Address(0x12)).unwrap(), "\"$0012\"");
}

#[test]
fn large_sources_are_split_into_bank_files() {
    let rom = nrom("Reset:\nNmi:\n  rti");
    let d = round_trip(&rom, &config(r#"{"options": {"split_banks": true}}"#));
    let names: Vec<_> = d.files().iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(
        names,
        ["out.asm", "out_bank00.asm", "out_bank01.asm"].map(PathBuf::from)
    );
    assert_lines(
        &d,
        &[
            "\t.inesmir 0",
            "",
            "\t.include \"out_bank00.asm\"",
            "\t.include \"out_bank01.asm\"",
        ],
    );
    let bank1 = &d.files()[2].1;
    assert!(bank1.lines().nth(1) == Some("\t.bank 1"), "{bank1}");
    assert!(
        d.files()
            .iter()
            .all(|(_, text)| disasm::is_unedited_source(text))
    );
    // Without the option, sources over 512 KiB are split.
    let mut rng = Lcg(7);
    let mut rom = vec![b'N', b'E', b'S', 0x1a, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    rom.extend((0..8 * 16384).map(|_| rng.next()));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_eq!(d.files().len(), 17);
    assert!(d.files().iter().all(|(_, text)| text.len() < 512 * 1024));
    let d = round_trip(&rom, &config(r#"{"options": {"split_banks": false}}"#));
    assert_eq!(d.files().len(), 1);
}

/// Disassembles a ROM hack of `base` (written as base.nes) and checks the
/// source reassembles to the hack.
fn patch_round_trip(rom: &[u8], base: &[u8], json: &str) -> Disassembly {
    let t = Temp::new();
    t.write("base.nes", base);
    let mut c = config(json);
    c.base = Some(PathBuf::from("base.nes"));
    let d = disasm::disassemble(rom, Some(base), &c, Path::new("out.asm"), &NEVER)
        .unwrap_or_else(|e| panic!("{e:?}"));
    let v = d.verify(&t.0, Some(&t.0), &NEVER);
    assert!(v.matched, "{v:#?}\n{}", d.text());
    d
}

const HACK_BASE: &str = concat!(
    "Reset:\n  lda #0\n  sta $2000\n  jsr Init\nLoop:\n  jmp Loop\n",
    "Init:\n  lda #1\n  rts\nNmi:\n  rti\nLoose:\n  .db 1, 2, 3\n",
);

fn hack_roms() -> (Vec<u8>, Vec<u8>) {
    let base = nrom(HACK_BASE);
    let hack = nrom(&format!(
        "{}\n  .org $E100\nNewInit:\n  lda #2\n  sta $0300\n  jmp Init",
        HACK_BASE.replace("jsr Init", "jsr NewInit")
    ));
    (base, hack)
}

#[test]
fn rom_hacks_disassemble_only_the_changes() {
    let (base, hack) = hack_roms();
    let d = patch_round_trip(&hack, &base, "{}");
    assert_lines(
        &d,
        &[
            "; The base ROM base.nes is included below; the changed ranges are written over it.",
            "; Labels in the unchanged ranges",
            "L_E00B          = $E00B",
            // The whole base without its header, then each range at its address.
            "\t.bank 0",
            "\t.org $C000",
            "\t.incbin \"base.nes\", 16, 16384",
            "",
            "\t.bank 1",
            "\t.org $E005",
            "\tjsr sub_E100",
            "",
            "\t.org $E100",
            "sub_E100:",
            "\tlda #$02",
            "\tsta $0300",
            "\tjmp L_E00B",
        ],
    );
    assert_eq!(d.text().matches(".incbin").count(), 1, "{}", d.text());
    assert!(!d.text().contains(".catbank"), "{}", d.text());
    // Bank 0 has no changes and no lines of its own.
    assert_eq!(d.text().matches(".bank").count(), 2, "{}", d.text());
    assert!(!d.text().contains("RESET:"), "{}", d.text());
    let diffs = d.diffs();
    assert_eq!(diffs.len(), 2);
    let (hook, new) = (&diffs[0], &diffs[1]);
    assert_eq!(
        (
            hook.bank,
            hook.start,
            hook.end,
            hook.changed_bytes,
            hook.code_bytes
        ),
        (1, Address(0xe005), Address(0xe007), 2, 3)
    );
    assert_eq!(
        (hook.bytes.as_str(), hook.base_bytes.as_str()),
        ("20 00 E1", "20 0B E0")
    );
    assert_eq!(hook.routines, ["RESET"]);
    assert_eq!(hook.base_fill, None);
    assert_eq!(
        (
            new.start,
            new.end,
            new.changed_bytes,
            new.base_fill.as_deref()
        ),
        (Address(0xe100), Address(0xe107), 7, Some("00"))
    );
    assert_eq!(new.labels, ["sub_E100"]);
    assert_eq!(new.referenced_from.len(), 1);
    assert_eq!(
        (
            new.referenced_from[0].address,
            new.referenced_from[0].line.as_str()
        ),
        (Address(0xe005), "jsr sub_E100")
    );
    let summary = d.summary().diff.unwrap();
    assert_eq!(
        (summary.changed_bytes, summary.ranges, summary.source_bytes),
        (9, 2, 11)
    );
    assert!(summary.new_banks.is_empty());
    // Issues outside the changes (the loose bytes) are left out.
    assert!(issues(&d, IssueKind::UnknownRange).is_empty());
    let full = round_trip(&hack, &DisasmConfig::default());
    assert_eq!(issues(&full, IssueKind::UnknownRange).len(), 1);
}

#[test]
fn patches_widen_the_disassembled_ranges() {
    let (base, hack) = hack_roms();
    let d = patch_round_trip(
        &hack,
        &base,
        r#"{"patches": [{"start": "$E002", "length": 2, "comment": "Hooked start-up"},
                        {"start": "$E00C", "end": "$E00D"}]}"#,
    );
    assert_lines(
        &d,
        &[
            "\t.incbin \"base.nes\", 16, 16384",
            "\t.bank 1",
            "; Hooked start-up",
            "\t.org $E002",
            "\tsta PPUCTRL",
            "\tjsr sub_E100",
            "",
            "\t.org $E00B",
            "\tlda #$01",
            "\trts",
            "",
            "\t.org $E100",
            "sub_E100:",
        ],
    );
    let diffs = d.diffs();
    // The second patch starts inside `lda #1` and is widened to it.
    assert_eq!(
        diffs.iter().map(|d| (d.start, d.end)).collect::<Vec<_>>(),
        [
            (Address(0xe002), Address(0xe007)),
            (Address(0xe00b), Address(0xe00d)),
            (Address(0xe100), Address(0xe107))
        ]
    );
    assert_eq!(diffs[1].changed_bytes, 0);
}

#[test]
fn expanded_rom_hacks_line_up_chr_banks() {
    let mut base = nrom(HACK_BASE);
    base[5] = 1;
    base.extend(vec![0x55; 8192]);
    // Two more PRG banks before the same CHR bank.
    let mut hack = base[..16].to_vec();
    hack[4] = 2;
    hack.extend(&base[16..16 + 16384]);
    hack.extend((0..16384u32).map(|i| (i % 7) as u8));
    hack.extend(&base[16 + 16384..]);
    let d = patch_round_trip(&hack, &base, "{}");
    let summary = d.summary().diff.unwrap();
    assert_eq!(summary.new_banks, [2, 3]);
    // The base's PRG banks, the new PRG banks, then the whole CHR ROM
    // (unchanged, but after the new PRG banks) from the CHR file.
    assert_lines(
        &d,
        &[
            "\t.bank 0",
            "\t.org $8000",
            "\t.incbin \"base.nes\", 16, 16384",
            "",
            "\t.bank 2",
            "\t.org $C000",
        ],
    );
    assert!(
        d.text()
            .ends_with("\n\n\t.bank 4\n\t.org $0000\n\t.incbin \"out.chr\"\n"),
        "{}",
        d.text()
    );
    assert_eq!(d.text().matches("\"base.nes\"").count(), 1);
    assert_eq!(
        d.chr_file(),
        Some((Path::new("out.chr"), &base[16 + 16384..]))
    );
    // Only the new PRG banks differ; the CHR bank is no change.
    assert_eq!(summary.changed_bytes, 16384);
    assert!(d.diffs().iter().all(|d| !d.chr));
}

/// Checks that instructions and directives are indented by one tab, and
/// that labels, constants and comments start the line.
fn assert_tab_indented(d: &Disassembly) {
    for (path, text) in d.files() {
        for line in text.lines() {
            let ok = match line.strip_prefix('\t') {
                Some(rest) => rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '.'),
                None => !line.starts_with(char::is_whitespace),
            };
            assert!(ok, "{}: {line:?}", path.display());
            if line.trim_start().starts_with('.') {
                assert!(line.starts_with("\t."), "{}: {line:?}", path.display());
            }
        }
    }
}

/// The paths a main source includes.
fn includes(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|l| l.strip_prefix("\t.include \""))
        .map(|l| PathBuf::from(l.trim_end_matches('"')))
        .collect()
}

#[test]
fn rom_hack_bank_files_overwrite_the_included_base() {
    let (base_prg, hack_prg) = hack_roms();
    let chr: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
    let mut base = base_prg.clone();
    base[5] = 1;
    base.extend(&chr);
    // Two new PRG banks before the CHR bank, and two changed CHR bytes.
    let mut hack = hack_prg[..16].to_vec();
    hack[4] = 2;
    hack[5] = 1;
    hack.extend(&hack_prg[16..]);
    hack.extend((0..16384u32).map(|i| (i % 5) as u8));
    let mut new_chr = chr.clone();
    new_chr[0x100] ^= 0xff;
    new_chr[0x1fff] = 0;
    hack.extend(&new_chr);
    let json = r#"{"options": {"split_banks": true},
        "banks": [{"bank": 0, "org": "$C000"}, {"bank": 1, "org": "$E000"},
                  {"bank": 2, "org": "$8000"}, {"bank": 3, "org": "$A000"}],
        "labels": [{"address": "$E100", "name": "NewInit", "comment": "Added"}],
        "patches": [{"start": "$E00B", "end": "$E00C", "comment": "Init"}]}"#;
    let d = patch_round_trip(&hack, &base, json);
    let names: Vec<PathBuf> = d.files().iter().map(|(p, _)| p.clone()).collect();
    // Bank 0 is unchanged and has no file; the CHR bank 4 has none at all.
    assert_eq!(
        names,
        [
            "out.asm",
            "out_bank01.asm",
            "out_bank02.asm",
            "out_bank03.asm"
        ]
        .map(PathBuf::from)
    );
    assert_eq!(includes(d.text()), names[1..]);
    assert_tab_indented(&d);
    // The main file includes the base's PRG banks once, and the whole CHR
    // ROM from the CHR file at its end.
    assert_lines(
        &d,
        &[
            "\t.inesprg 2",
            "\t.bank 0",
            "\t.org $C000",
            "\t.incbin \"base.nes\", 16, 16384",
            "",
            "\t.include \"out_bank01.asm\"",
            "\t.include \"out_bank03.asm\"",
            "",
            "\t.bank 4",
            "\t.org $0000",
            "\t.incbin \"out.chr\"",
        ],
    );
    assert!(
        d.text().ends_with(
            "\t.include \"out_bank03.asm\"\n\n\t.bank 4\n\t.org $0000\n\t.incbin \"out.chr\"\n"
        ),
        "{}",
        d.text()
    );
    assert_eq!(d.text().matches("\"base.nes\"").count(), 1);
    assert_eq!(d.text().matches("\"out.chr\"").count(), 1);
    assert!(!d.text().contains("L_E00B          ="), "{}", d.text());
    for (path, text) in &d.files()[1..] {
        assert!(!text.contains("base.nes"), "{}: {text}", path.display());
        assert!(!text.contains("out.chr"), "{}: {text}", path.display());
    }
    let bank1 = &d.files()[1].1;
    let lines: Vec<&str> = bank1.lines().skip(1).collect();
    assert_eq!(
        lines,
        [
            "\t.bank 1",
            "\t.org $E005",
            "\tjsr NewInit",
            "",
            "; Init",
            "\t.org $E00B",
            // Defined in the range, so not a constant of the main file.
            "L_E00B:",
            "\tlda #$01",
            "",
            "\t.org $E100",
            "; Added",
            "NewInit:",
            "\tlda #$02",
            "\tsta $0300",
            "\tjmp L_E00B",
        ]
    );
    // New banks are written whole; the CHR bank comes from the CHR file.
    assert!(d.files()[2].1.contains("\t.bank 2\n\t.org $8000\n"));
    assert_eq!(
        d.chr_file(),
        Some((Path::new("out.chr"), new_chr.as_slice()))
    );
    assert!(
        d.files()
            .iter()
            .all(|(_, text)| disasm::is_unedited_source(text))
    );
    // As one file, the CHR bank is included from the CHR file too.
    let d = patch_round_trip(
        &hack,
        &base,
        &json.replace(r#""split_banks": true"#, r#""split_banks": false"#),
    );
    assert_eq!(d.files().len(), 1);
    assert_tab_indented(&d);
    assert!(
        d.text()
            .ends_with("\t.bank 4\n\t.org $0000\n\t.incbin \"out.chr\"\n"),
        "{}",
        d.text()
    );
    assert_eq!(
        d.chr_file(),
        Some((Path::new("out.chr"), new_chr.as_slice()))
    );
    // A ROM hack always includes its CHR ROM from the CHR file
    // (edited with CHR tools), never as `.db` lines.
    for split in ["true", "false"] {
        let mut c = config(&json.replace(
            r#""split_banks": true"#,
            &format!(r#""split_banks": {split}, "chr": "db""#),
        ));
        c.base = Some(PathBuf::from("base.nes"));
        let e = disasm::disassemble(&hack, Some(&base), &c, Path::new("out.asm"), &NEVER)
            .err()
            .expect("chr db with base should fail");
        let messages: Vec<&str> = e.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "options.chr \"db\" does not apply with base: the CHR ROM is included from the CHR file"
            ]
        );
    }
}

#[test]
fn raw_rom_hacks_name_bank_files_in_hexadecimal() {
    let t = Temp::new();
    // 18 banks, each filled with its number.
    let base: Vec<u8> = (0..18 * 8192).map(|i| (i / 8192) as u8).collect();
    let mut hack = base.clone();
    // Two changes far apart in bank 10, one in banks 16 and 17, and a new bank 18.
    hack[10 * 8192 + 0x100..10 * 8192 + 0x103].copy_from_slice(&[1, 2, 3]);
    hack[10 * 8192 + 0x1800] = 4;
    hack[16 * 8192 + 5] = 5;
    hack[17 * 8192 + 0x1ff0] = 6;
    hack.extend(vec![0xea; 8192]);
    t.write("base.bin", &base);
    let c = config(r#"{"base": "base.bin", "options": {"split_banks": true}}"#);
    let d = disasm::disassemble(&hack, Some(&base), &c, Path::new("src/hack.asm"), &NEVER)
        .unwrap_or_else(|e| panic!("{e:?}"));
    let v = d.verify(&t.0, Some(&t.0), &NEVER);
    assert!(v.matched, "{v:#?}");
    let names: Vec<PathBuf> = d.files().iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(
        names,
        [
            "src/hack.asm",
            "src/hack_bank0A.asm",
            "src/hack_bank10.asm",
            "src/hack_bank11.asm",
            "src/hack_bank12.asm"
        ]
        .map(PathBuf::from)
    );
    assert_eq!(includes(d.text()), names[1..]);
    assert_tab_indented(&d);
    assert_lines(
        &d,
        &[
            "; Assemble with -raw (no iNES header).",
            "\t.bank 0",
            "\t.org $8000",
            "\t.incbin \"base.bin\", 0, 147456",
        ],
    );
    assert_eq!(d.text().matches(".incbin").count(), 1);
    // Each file holds its bank, by number; the name is hexadecimal.
    for (name, bank) in [(1, 10), (2, 16), (3, 17), (4, 18)] {
        let text = &d.files()[name].1;
        assert_eq!(
            text.lines().nth(1),
            Some(format!("\t.bank {bank}").as_str()),
            "{text}"
        );
    }
    let bank10 = &d.files()[1].1;
    let orgs: Vec<&str> = bank10.lines().filter(|l| l.contains(".org")).collect();
    assert_eq!(orgs, ["\t.org $8100", "\t.org $9800"]);
    assert!(d.files()[3].1.contains("\t.org $DFF0\n"));
    assert_eq!(
        disasm::bank_path(Path::new("a/x.asm"), 63),
        Path::new("a/x_bank3F.asm")
    );
    assert_eq!(d.summary().diff.unwrap().new_banks, [18]);
}

#[test]
fn rom_hack_annotation_errors() {
    let (base, hack) = hack_roms();
    let fails = |rom: &[u8], base_file: Option<&[u8]>, json: &str, message: &str| {
        let e = disasm::disassemble(rom, base_file, &config(json), Path::new("out.asm"), &NEVER)
            .err()
            .unwrap_or_else(|| panic!("{json} should fail"));
        assert!(
            e.iter().any(|d| d.message.contains(message)),
            "{json}: {:?}",
            e.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    };
    fails(
        &hack,
        None,
        r#"{"patches": [{"start": "$E000", "length": 1}]}"#,
        "patches applies only with base",
    );
    fails(
        &hack,
        None,
        r#"{"base": "base.nes"}"#,
        "The base ROM was not given",
    );
    fails(
        &hack,
        Some(&base),
        "{}",
        "A base ROM was given without base",
    );
    fails(
        &hack,
        Some(&base[16..]),
        r#"{"base": "base.bin"}"#,
        "base must have the same format as rom",
    );
    fails(
        &hack,
        Some(&base),
        r#"{"base": "base.nes", "patches": [{"start": "$1000", "length": 1}]}"#,
        "patches[0] $1000: No PRG bank holds $1000",
    );
}

#[test]
fn trainers_fail_verification() {
    let mut rom = nrom("Reset:\nNmi:\n  rti");
    rom[6] |= 0x04;
    let trainer: Vec<u8> = (0..512u32).map(|i| i as u8).collect();
    rom.splice(16..16, trainer);
    let d = disasm::disassemble(
        &rom,
        None,
        &DisasmConfig::default(),
        Path::new("out.asm"),
        &NEVER,
    )
    .unwrap();
    assert_lines(&d, &["\t.inesmir 4"]);
    let t = Temp::new();
    let v = d.verify(&t.0, Some(&t.0), &NEVER);
    assert!(!v.matched);
    assert_eq!(v.mismatch_count, 512);
    let m = &v.mismatches[0];
    assert_eq!(
        (m.bank, m.address, m.actual.as_str()),
        (None, Address(16), "none")
    );
}

#[test]
fn huge_annotation_values_are_errors() {
    let rom = nrom("Reset:\n  jsr Sub\nNmi:\n  rti\nSub:\n  rts");
    for (json, message) in [
        (
            r#"{"regions": [{"start": "$E000", "end": "$FFFFFFFF"}]}"#,
            "regions[0] $E000: the range runs past the end of bank 1 ($FFFF)",
        ),
        (
            r#"{"entry_points": [{"address": "$E004", "inline_bytes": 18446744073709551615}]}"#,
            "entry_points[0] $E004: inline_bytes must be at most 8192",
        ),
        (
            r#"{"symbols": [{"name": "buf", "value": 1, "size": 4294967295}]}"#,
            "symbols[0] $0001 (buf): Symbol buf covers addresses past $FFFF",
        ),
    ] {
        let e = disasm::disassemble(&rom, None, &config(json), Path::new("out.asm"), &NEVER)
            .err()
            .unwrap_or_else(|| panic!("{json} should fail"));
        assert!(
            e.iter().any(|d| d.message == message),
            "{json}: {:?}",
            e.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }
    let bin: Vec<u8> = vec![0xea; 16];
    let e = disasm::disassemble(
        &bin,
        None,
        &config(r#"{"load_address": 0, "regions": [{"start": 0, "end": "$FFFFFFFF"}]}"#),
        Path::new("out.asm"),
        &NEVER,
    )
    .err()
    .unwrap();
    assert!(
        e[0].message
            .contains("the range runs past the end of bank 0"),
        "{e:?}"
    );
}

#[test]
fn paths_cannot_add_source_lines() {
    let rom = nrom("Reset:\nNmi:\n  rti");
    let c = DisasmConfig {
        rom: PathBuf::from("a\n  .include \"extra.asm\"\n;b.nes"),
        ..Default::default()
    };
    let d = round_trip(&rom, &c);
    assert!(
        d.text()
            .lines()
            .any(|l| l == "; Source: a .include \"extra.asm\" ;b.nes"),
        "{}",
        d.text()
    );
    for (source, base) in [("out\".asm", None), ("out.asm", Some("base\n.nes"))] {
        let c = DisasmConfig {
            base: base.map(PathBuf::from),
            ..Default::default()
        };
        let e = disasm::disassemble(&rom, Some(&rom), &c, Path::new(source), &NEVER)
            .err()
            .unwrap();
        assert!(
            e[0].message
                .contains("contains a quote, control character or backslash"),
            "{e:?}"
        );
    }
}

#[test]
fn names_in_operand_overrides_are_defined() {
    let rom = nrom("Reset:\n  lda #0\n  sta $2001\nNmi:\n  rti");
    // PPUMASK is used only by the override.
    let d = round_trip(
        &rom,
        &config(
            r#"{"operands": [{"address": "$E002", "expr": "PPUMASK"}], "options": {"nes_registers": true}}"#,
        ),
    );
    assert_lines(&d, &["PPUMASK         = $2001", "\tsta PPUMASK"]);
    // In a ROM hack, a label of an unchanged range named only by an override.
    let (base, hack) = hack_roms();
    let d = patch_round_trip(
        &hack,
        &base,
        r#"{"labels": [{"address": "$E00E", "name": "Handler"}],
            "operands": [{"address": "$E105", "expr": "Handler-3"}]}"#,
    );
    assert_lines(&d, &["Handler         = $E00E", "\tjmp Handler-3"]);
}

#[test]
fn adjusted_pointers_that_wrap_are_numbers() {
    let mut c = config(
        r#"{"load_address": "$0300", "symbols": [{"name": "Last", "value": "$FFFF"}],
            "regions": [{"start": "$0300", "length": 2, "type": "pointers", "adjust": 514},
                        {"start": "$0302", "length": 1, "type": "low_bytes", "pair": "$0303", "adjust": 514},
                        {"start": "$0303", "length": 1, "type": "high_bytes"}]}"#,
    );
    c.rom = PathBuf::from("pointer.bin");
    let d = round_trip(&[0x01, 0x02, 0x01, 0x02], &c);
    assert_lines(&d, &["\t.dw $0201", "\t.db $01", "\t.db $02"]);
}

#[test]
fn rom_hack_banks_may_name_new_banks() {
    let base = nrom(HACK_BASE);
    let mut hack = base[..16].to_vec();
    hack[4] = 2;
    hack.extend(&base[16..]);
    hack.extend(vec![0x60; 16384]);
    // Bank 3 exists only in the hack.
    let d = patch_round_trip(&hack, &base, r#"{"banks": [{"bank": 3, "org": "$A000"}]}"#);
    assert_eq!(d.summary().diff.unwrap().new_banks, [2, 3]);
}

#[test]
fn edits_inside_lines_change_the_checksum() {
    let rom = nrom("Reset:\nNmi:\n  rti\n  .db \"ww\"");
    let d = round_trip(
        &rom,
        &config(r#"{"regions": [{"start": "$E001", "length": 2, "type": "text"}]}"#),
    );
    let text = d.text();
    assert!(disasm::is_unedited_source(text));
    assert!(disasm::is_unedited_source(&text.replace('\n', "\r\n")));
    // A carriage return inside a line changes the bytes.
    let edited = text.replacen("\"ww\"", "\"w\rw\"", 1);
    assert_ne!(edited, text);
    assert!(!disasm::is_unedited_source(&edited));
}

#[test]
fn every_name_in_an_override_is_defined() {
    let (base, hack) = hack_roms();
    // Two names for one address of an unchanged range; the override uses the second.
    let d = patch_round_trip(
        &hack,
        &base,
        r#"{"labels": [{"address": "$E00B", "name": "First"}, {"address": "$E00B", "name": "Second"}],
            "operands": [{"address": "$E105", "expr": "Second"}]}"#,
    );
    // Only the name the source uses needs a definition.
    assert_lines(&d, &["Second          = $E00B", "\tjmp Second"]);
    assert!(!d.text().contains("First"), "{}", d.text());
}

#[test]
fn overrides_apply_in_text_regions() {
    let rom = nrom("Reset:\nNmi:\n  rti\nMsg:\n  .db \"HI\"");
    let regions = r#""regions": [{"start": "$E001", "length": 2, "type": "text"}]"#;
    let d = round_trip(
        &rom,
        &config(&format!(
            r#"{{{regions}, "operands": [{{"address": "$E001", "expr": "'H'"}}]}}"#
        )),
    );
    assert_lines(&d, &["\t.db 'H'", "\t.db \"I\""]);
    // A value that changes the byte fails verification.
    let d = disasm::disassemble(
        &rom,
        None,
        &config(&format!(
            r#"{{{regions}, "operands": [{{"address": "$E001", "expr": "$00"}}]}}"#
        )),
        Path::new("out.asm"),
        &NEVER,
    )
    .unwrap();
    let t = Temp::new();
    let v = d.verify(&t.0, Some(&t.0), &NEVER);
    assert_eq!(v.mismatch_count, 1);
    assert_eq!(
        (
            v.mismatches[0].expected.as_str(),
            v.mismatches[0].actual.as_str()
        ),
        ("48", "00")
    );
}

#[test]
fn routines_continue_into_the_next_bank() {
    // NOP at $DFFF (bank 0), then LDA <$20 / RTS at $E000 (bank 1).
    let c = config(
        r#"{"load_address": "$DFFF", "entry_points": [{"address": "$DFFF", "name": "Split"}]}"#,
    );
    let d = round_trip(&[0xea, 0xa5, 0x20, 0x60], &c);
    let split = d
        .routines()
        .into_iter()
        .find(|r| r.name == "Split")
        .unwrap();
    assert_eq!(
        (split.size, split.reads.clone()),
        (4, vec!["$0020".to_owned()])
    );
}

#[test]
fn long_routines_are_marked_truncated() {
    let mut code = vec![0xea; 16385];
    code.push(0x60);
    let c = config(
        r#"{"load_address": "$8000", "entry_points": [{"address": "$8000", "name": "Long"}]}"#,
    );
    let d = round_trip(&code, &c);
    let long = d.routines().into_iter().find(|r| r.name == "Long").unwrap();
    assert_eq!((long.size, long.truncated), (16384, true));
    let json = serde_json::to_value(d.routines()).unwrap();
    assert_eq!(json[0]["truncated"], true);
    let short = round_trip(&[0xea, 0x60], &c);
    assert!(
        serde_json::to_value(short.routines()).unwrap()[0]
            .get("truncated")
            .is_none()
    );
}

#[test]
fn ram_lists_every_routine_sharing_code() {
    let rom = nrom(concat!(
        "Reset:\n  jsr One\n  jsr Two\n  jmp Reset\n",
        "One:\n  jmp Tail\nTwo:\n  jmp Tail\nTail:\n  lda <$20\n  rts\nNmi:\n  rti",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    let ram = d.ram();
    let zp = ram.iter().find(|r| r.address == Address(0x20)).unwrap();
    assert_eq!(zp.routines, ["sub_E009", "sub_E00C"]);
}

#[test]
fn rom_hack_issues_overlapping_a_change_are_kept() {
    let base = nrom(HACK_BASE);
    let mut hack = base.clone();
    // A byte inside the loose data at $E00F-$E011.
    hack[16 + 8192 + 0x10] = 9;
    let d = patch_round_trip(&hack, &base, "{}");
    assert_eq!(
        d.diffs()
            .iter()
            .map(|d| (d.start, d.end))
            .collect::<Vec<_>>(),
        [(Address(0xe010), Address(0xe010))]
    );
    assert_eq!(
        issues(&d, IssueKind::UnknownRange),
        ["$E00F $E00F-$E011 (3 bytes) is not reached by code nor described by a region"]
    );
    // The issue after a change is left out.
    let mut hack = base.clone();
    hack[16 + 8192 + 0x0e] = 0x40;
    let d = patch_round_trip(&hack, &base, "{}");
    assert!(issues(&d, IssueKind::UnknownRange).is_empty());
}

#[test]
fn changed_chr_banks_count_as_data() {
    let mut base = nrom(HACK_BASE);
    base[5] = 1;
    base.extend(vec![0x55; 8192]);
    let mut hack = base.clone();
    let last = hack.len() - 1;
    hack[last] = 0;
    let d = patch_round_trip(&hack, &base, "{}");
    let chr = d.diffs().into_iter().find(|d| d.chr).unwrap();
    assert_eq!(
        (
            chr.changed_bytes,
            chr.data_bytes,
            chr.code_bytes,
            chr.unknown_bytes
        ),
        (1, 8192, 0, 0)
    );
    // The CHR file holds the whole CHR ROM, but only the changed byte counts.
    assert_eq!(d.summary().diff.unwrap().changed_bytes, 1);
    assert_eq!(d.chr_file().unwrap().1, &hack[16 + 16384..]);
}

#[test]
fn rom_hacks_with_chr_ram_have_no_chr_file() {
    let (base, hack) = hack_roms();
    assert_eq!(hack[5], 0);
    for split in ["true", "false"] {
        let json = format!(r#"{{"options": {{"split_banks": {split}}}}}"#);
        let d = patch_round_trip(&hack, &base, &json);
        assert!(d.chr_file().is_none());
        assert!(d.files().iter().all(|(_, text)| !text.contains(".chr")));
        assert!(!d.text().contains("CHR file checksum"), "{}", d.text());
    }
}

#[cfg(not(windows))]
#[test]
fn backslashes_are_not_separators_outside_windows() {
    let rom = nrom("Reset:\nNmi:\n  rti");
    let e = disasm::disassemble(
        &rom,
        None,
        &DisasmConfig::default(),
        Path::new("a\\b.asm"),
        &NEVER,
    )
    .err()
    .unwrap();
    assert!(
        e[0].message
            .contains("contains a quote, control character or backslash"),
        "{e:?}"
    );
}

#[test]
fn words_are_not_split_by_names_inside_them() {
    let c = config(
        r#"{"load_address": "$7000", "regions": [{"start": "$7000", "length": 4, "type": "words"}],
            "labels": [{"address": "$7001", "name": "Mid"}],
            "comments": [{"address": "$7001", "text": "high byte"}],
            "operands": [{"address": "$7000", "expr": "$7777"}]}"#,
    );
    let d = round_trip(&[0x77; 4], &c);
    assert_lines(
        &d,
        &[
            "D_7000:",
            "\t.dw $7777, $7777 ; high byte",
            "Mid = D_7000+1",
        ],
    );
    // Reads of the second byte of a word name the word.
    let rom = nrom("Reset:\n  lda Table+1\nNmi:\n  rti\nTable:\n  .dw $1234, $5678");
    let d = round_trip(
        &rom,
        &config(r#"{"regions": [{"start": "$E004", "length": 4, "type": "words"}]}"#),
    );
    assert_lines(&d, &["\tlda D_E004+1", "D_E004:", "\t.dw $1234, $5678"]);
    // An expression cannot replace half a word.
    let e = disasm::disassemble(
        &[0x77; 4],
        None,
        &config(
            r#"{"load_address": "$7000", "regions": [{"start": "$7000", "length": 4, "type": "words"}],
                "operands": [{"address": "$7001", "expr": "$77"}]}"#,
        ),
        Path::new("out.asm"),
        &NEVER,
    )
    .err()
    .unwrap();
    assert_eq!(
        e[0].message,
        "operands $7001: the second byte of a word; give the expression at the word"
    );
}

#[test]
fn inline_arguments_may_be_described_by_regions() {
    let rom = nrom(concat!(
        "Reset:\n  jsr Sub\n  .db $41\n  lda <$20\n  rts\n",
        "Sub:\n  nop\n  rts\nNmi:\n  rti",
    ));
    let d = round_trip(
        &rom,
        &config(
            r#"{"entry_points": [{"address": "$E007", "inline_bytes": 1}],
                "regions": [{"start": "$E003", "length": 1, "type": "text"}]}"#,
        ),
    );
    assert_lines(
        &d,
        &["\tjsr sub_E007", "\t.db \"A\"", "\tlda <$20", "\trts"],
    );
    assert!(issues(&d, IssueKind::CodeDataConflict).is_empty());
    let reset = d
        .routines()
        .into_iter()
        .find(|r| r.name == "RESET")
        .unwrap();
    assert_eq!(reset.reads, ["$0020"]);
}

#[test]
fn rom_hack_issues_cover_whole_instructions() {
    // JSR at $E000 would overlap the data region at $E002.
    let rom = nrom("Reset:\n  jsr Sub\nNmi:\n  rti\nSub:\n  rts");
    let d = patch_round_trip(
        &rom,
        &rom,
        r#"{"regions": [{"start": "$E002", "length": 1}],
            "patches": [{"start": "$E001", "length": 1}]}"#,
    );
    assert_eq!(
        issues(&d, IssueKind::Overlap),
        [
            "$E000 The instruction at $E000 (from $FFFC) would overlap other code, data or the end of the bank"
        ]
    );
}

#[test]
fn vectors_are_traced_whatever_regions_cover() {
    let rom = nrom("Reset:\n  lda #1\nNmi:\n  rti");
    // A region over the NMI vector only.
    let d = round_trip(
        &rom,
        &config(r#"{"regions": [{"start": "$FFFA", "length": 2, "type": "words"}]}"#),
    );
    assert_lines(
        &d,
        &[
            "RESET:",
            "\tlda #$01",
            "NMI:",
            "\trti",
            "\t.dw $E002",
            "\t.dw RESET, NMI",
        ],
    );
    // The automatic vector table is a pointer table too.
    let e = disasm::disassemble(
        &rom,
        None,
        &config(r#"{"operands": [{"address": "$FFFB", "expr": "$00"}]}"#),
        Path::new("out.asm"),
        &NEVER,
    )
    .err()
    .unwrap();
    assert_eq!(
        e[0].message,
        "operands $FFFB: the second byte of a word; give the expression at the word"
    );
}

#[test]
fn rom_hack_table_issues_include_the_high_byte() {
    // Banks 0 and 1 both at $8000; the table in bank 3 points to $8000.
    let mut bin = vec![0u8; 4 * 8192];
    bin[3 * 8192 + 0x10] = 0x80;
    let json = r#"{"banks": [{"bank": 1, "org": "$8000"}],
        "regions": [{"start": "$E000", "length": 1, "type": "low_bytes", "pair": "$E010"},
                    {"start": "$E010", "length": 1, "type": "high_bytes"}],
        "patches": [{"start": "$E010", "length": 1}]}"#;
    let t = Temp::new();
    t.write("base.bin", &bin);
    let mut c = config(json);
    c.base = Some(PathBuf::from("base.bin"));
    let d = disasm::disassemble(&bin, Some(&bin), &c, Path::new("out.asm"), &NEVER).unwrap();
    assert!(d.verify(&t.0, Some(&t.0), &NEVER).matched);
    assert_eq!(
        issues(&d, IssueKind::UnresolvedBank),
        ["$E000 $8000 is held by banks 0, 1; add a references entry with target_bank"]
    );
}

#[test]
fn labels_inside_words_and_instructions_keep_comments() {
    let c = config(
        r#"{"load_address": "$7000", "regions": [{"start": "$7000", "length": 2, "type": "words"}],
            "labels": [{"address": "$7001", "name": "Mid", "comment": "keep this note"}]}"#,
    );
    let d = round_trip(&[0x77; 2], &c);
    assert_lines(&d, &["\t.dw $7777", "; keep this note", "Mid = D_7000+1"]);
    let rom = nrom(concat!(
        "First:\n  lda #1\n  .db $2C\nSecond:\n  lda #2\n  rts\n",
        "Reset:\n  jsr First\n  jsr Second\n  jmp Reset\nNmi:\n  rti",
    ));
    let d = round_trip(
        &rom,
        &config(
            r#"{"labels": [{"address": "$E003", "name": "Second", "comment": "skipped by BIT"}]}"#,
        ),
    );
    assert_lines(
        &d,
        &["\tbit $02A9", "; skipped by BIT", "Second = L_E002+1"],
    );
}

#[test]
fn vector_references_are_reported_once() {
    // Banks 0 and 1 both at $8000; the NMI vector in bank 3 points to $8000.
    let mut bin = vec![0u8; 4 * 8192];
    bin[3 * 8192 + 0x1ffa] = 0x00;
    bin[3 * 8192 + 0x1ffb] = 0x80;
    let banks = r#""banks": [{"bank": 1, "org": "$8000"}]"#;
    let expected = ["$FFFA $8000 is held by banks 0, 1; add a references entry with target_bank"];
    for regions in [
        "[]",
        r#"[{"start": "$FFFA", "length": 2, "type": "bytes"}]"#,
        r#"[{"start": "$FFFA", "length": 6, "type": "pointers"}]"#,
        r#"[{"start": "$FFFA", "length": 6, "type": "pointers", "code_targets": true}]"#,
        // Tables that read other values than the vector: misaligned or adjusted.
        r#"[{"start": "$FFF9", "length": 2, "type": "pointers"}]"#,
        r#"[{"start": "$FFFA", "length": 6, "type": "pointers", "adjust": 1}]"#,
    ] {
        let d = round_trip(
            &bin,
            &config(&format!(r#"{{{banks}, "regions": {regions}}}"#)),
        );
        assert_eq!(issues(&d, IssueKind::UnresolvedBank), expected, "{regions}");
        assert_eq!(d.xrefs("$8000").unwrap().len(), 1, "{regions}");
    }
    // Either byte of the vector in a ROM hack range keeps the issue, whether
    // the automatic table, a pointers region or a bytes region holds it.
    let t = Temp::new();
    t.write("base.bin", &bin);
    for regions in [
        "[]",
        r#"[{"start": "$FFFA", "length": 6, "type": "pointers"}]"#,
        r#"[{"start": "$FFFA", "length": 2, "type": "bytes"}]"#,
    ] {
        for patch in ["$FFFA", "$FFFB"] {
            let mut c = config(&format!(
                r#"{{{banks}, "regions": {regions},
                    "patches": [{{"bank": 3, "start": "{patch}", "length": 1}}]}}"#
            ));
            c.base = Some(PathBuf::from("base.bin"));
            let d =
                disasm::disassemble(&bin, Some(&bin), &c, Path::new("out.asm"), &NEVER).unwrap();
            assert!(d.verify(&t.0, Some(&t.0), &NEVER).matched);
            assert_eq!(
                issues(&d, IssueKind::UnresolvedBank),
                expected,
                "{regions} {patch}"
            );
        }
    }
}

/// A mapper 4 ROM of four PRG banks. With `straddle`, the code of bank 2
/// ends with `lda $0390,x` at $DFFE, whose last byte is the first of bank 3
/// ($E000); otherwise it ends with `nop` at $DFFF.
fn mmc3_rom(straddle: bool) -> Vec<u8> {
    let (tail, next) = if straddle {
        ("  .db $BD, $90\n", "  .db $03\n")
    } else {
        ("  inx\n  nop\n", "")
    };
    assemble(
        &[
            "  .inesprg 2\n  .inesmap 4\n  .bank 2\n  .org $DFF0\n",
            "Reset:\n  sei\n  ldx #$00\n  jsr Init\n  lda $0300\n  sta $0301\n  inx\n  iny\n",
            tail,
            "  .bank 3\n  .org $E000\n",
            next,
            "  beq Reset\n  jsr Init\nLoop:\n  jmp Loop\n",
            "  .org $E010\nInit:\n  lda $0300\n  rts\nNmi:\n  rti\n",
            "  .org $FFFA\n  .dw Nmi, Reset, Nmi\n",
        ]
        .concat(),
    )
}

const FIXED_PAIR: &str = r#""mapper4_fixed_prg_16k": true"#;

#[test]
fn mapper4_fixed_prg_16k_decodes_across_the_last_two_banks() {
    let rom = mmc3_rom(true);
    // By default, the instruction would overlap the end of bank 2.
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_eq!(
        issues(&d, IssueKind::Overlap),
        [
            "$DFFE The instruction at $DFFE (from $DFFD) would overlap other code, data or the end of the bank"
        ]
    );
    assert!(!d.text().contains("lda $0390,x"), "{}", d.text());
    assert!(!d.text().contains(".catbank"), "{}", d.text());
    let d = round_trip(
        &rom,
        &config(&format!(r#"{{"options": {{{FIXED_PAIR}}}}}"#)),
    );
    assert!(
        d.issues().iter().all(|i| i.kind != IssueKind::Overlap),
        "{:?}",
        d.issues()
    );
    assert_tab_indented(&d);
    assert_lines(
        &d,
        &[
            "\t.catbank $02",
            "\t.bank 2",
            "\t.org $C000",
            "RESET:",
            "\tjsr sub_E010",
            "\tiny",
            "\tlda $0390,x",
            "; Bank 3 starts at $E000, inside the instruction above",
            // The code goes on in bank 3, and branches back to bank 2.
            "\tbeq RESET",
            "\tjsr sub_E010",
            "L_E006:",
            "\tjmp L_E006",
        ],
    );
    // No `.bank` or `.org` splits the instruction.
    assert!(!d.text().contains("\t.bank 3"), "{}", d.text());
    assert!(!d.text().contains(".org $E000"), "{}", d.text());
    let line = d.view(Some(2), 0xdffe, 0xdffe)[0];
    assert_eq!(
        (line.size, line.bytes.as_str(), line.text.as_str()),
        (3, "BD 90 03", "\tlda $0390,x")
    );
    let texts: Vec<&str> = d
        .view(Some(3), 0xe000, 0xe002)
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        texts,
        [
            "; Bank 3 starts at $E000, inside the instruction above",
            "\tbeq RESET"
        ]
    );
    let banks = d.summary().banks;
    assert_eq!((banks[2].code_bytes, banks[3].code_bytes), (16, 14));
    // The routine runs across the boundary, and the branch back refers to bank 2.
    let routines = d.routines();
    let reset = routines.iter().find(|r| r.name == "RESET").unwrap();
    assert_eq!(
        (
            reset.bank,
            reset.address,
            reset.size,
            reset.calls.as_slice()
        ),
        (2, Address(0xdff0), 25, ["sub_E010".to_owned()].as_slice())
    );
    let branch = d.xrefs("RESET").unwrap();
    assert!(
        branch
            .iter()
            .any(|x| (x.bank, x.address, x.kind, x.line.as_str())
                == (3, Address(0xe001), XrefKind::Branch, "beq RESET")),
        "{branch:?}"
    );
    // A name inside the instruction is defined relative to it.
    let d = round_trip(
        &rom,
        &config(&format!(
            r#"{{"labels": [{{"address": "$E000", "name": "Inner"}}], "options": {{{FIXED_PAIR}}}}}"#
        )),
    );
    assert_lines(
        &d,
        &[
            "L_DFFE:",
            "\tlda $0390,x",
            "Inner = L_DFFE+2",
            "; Bank 3 starts at $E000, inside the instruction above",
        ],
    );
}

#[test]
fn mapper4_fixed_prg_16k_writes_one_file_for_the_last_two_banks() {
    let rom = mmc3_rom(true);
    let split = config(&format!(
        r#"{{"options": {{{FIXED_PAIR}, "split_banks": true}}}}"#
    ));
    let d = round_trip(&rom, &split);
    let names: Vec<PathBuf> = d.files().iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(
        names,
        [
            "out.asm",
            "out_bank00.asm",
            "out_bank01.asm",
            "out_bank02_03.asm"
        ]
        .map(PathBuf::from)
    );
    assert_eq!(includes(d.text()), names[1..]);
    // `.catbank` in the main file lets bank 2 continue into bank 3.
    assert_lines(
        &d,
        &[
            "",
            "\t.include \"out_bank00.asm\"",
            "\t.include \"out_bank01.asm\"",
            "\t.catbank $02",
            "\t.include \"out_bank02_03.asm\"",
        ],
    );
    assert_eq!(d.text().matches(".catbank").count(), 1);
    let pair = &d.files()[3].1;
    assert_eq!(pair.lines().nth(1), Some("\t.bank 2"), "{pair}");
    assert!(!pair.contains(".catbank"), "{pair}");
    assert!(!pair.contains("\t.bank 3"), "{pair}");
    assert!(
        pair.contains(
            "\tlda $0390,x\n; Bank 3 starts at $E000, inside the instruction above\n\tbeq RESET\n"
        ),
        "{pair}"
    );
    assert_tab_indented(&d);
    assert!(
        d.files()
            .iter()
            .all(|(_, text)| disasm::is_unedited_source(text))
    );
    assert_eq!(
        disasm::bank_pair_path(Path::new("a/x.asm"), 0x1e),
        Path::new("a/x_bank1E_1F.asm")
    );
    // Without the option, each bank has its file.
    let d = round_trip(&rom, &config(r#"{"options": {"split_banks": true}}"#));
    let names: Vec<PathBuf> = d.files().iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(
        names[3..],
        ["out_bank02.asm", "out_bank03.asm"].map(PathBuf::from)
    );
    assert!(!d.text().contains(".catbank"), "{}", d.text());
    // Between whole instructions, bank 3 starts with `.bank` and `.org`.
    let d = round_trip(&mmc3_rom(false), &split);
    let pair = &d.files()[3].1;
    assert!(
        pair.contains("\tnop\n\n\t.bank 3\n\t.org $E000\n\tbeq RESET\n"),
        "{pair}"
    );
    assert_lines(&d, &["\t.catbank $02", "\t.include \"out_bank02_03.asm\""]);
}

#[test]
fn mapper4_fixed_prg_16k_needs_a_mapper_4_rom_and_agreeing_banks() {
    let rom = mmc3_rom(true);
    let fails = |rom: &[u8], json: &str, message: &str| {
        let e = disasm::disassemble(rom, None, &config(json), Path::new("out.asm"), &NEVER)
            .err()
            .unwrap_or_else(|| panic!("{json} should fail"));
        let messages: Vec<&str> = e.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages, [message], "{json}");
    };
    let option = format!(r#"{{"options": {{{FIXED_PAIR}}}}}"#);
    fails(
        &nrom("Reset:\nNmi:\n  rti"),
        &option,
        "options.mapper4_fixed_prg_16k applies only to mapper 4 (MMC3); the ROM uses mapper 0",
    );
    fails(
        &rom[16..],
        &option,
        "options.mapper4_fixed_prg_16k applies only to iNES ROMs of mapper 4 (MMC3)",
    );
    let banks = |layout: &str| format!(r#"{{"banks": [{layout}], "options": {{{FIXED_PAIR}}}}}"#);
    fails(
        &rom,
        &banks(r#"{"bank": 2, "org": "$8000"}"#),
        "banks[0]: banks gives bank 2 org $8000, but options.mapper4_fixed_prg_16k places it at $C000",
    );
    fails(
        &rom,
        &banks(r#"{"bank": 3, "org": "$A000"}"#),
        "banks[0]: banks gives bank 3 org $A000, but options.mapper4_fixed_prg_16k places it at $E000",
    );
    // The windows the option sets may also be given.
    round_trip(
        &rom,
        &config(&banks(
            r#"{"bank": 2, "org": "$C000"}, {"bank": 3, "org": "$E000"}"#,
        )),
    );
    // 16 KiB of PRG ROM in the header, but less in the file.
    let mut short = rom[..16].to_vec();
    short[4] = 1;
    short.extend(&rom[16..16 + 8192]);
    fails(
        &short,
        &option,
        "options.mapper4_fixed_prg_16k needs at least two 8 KiB PRG banks; the ROM has 1",
    );
    short.extend(&rom[16..116]);
    fails(
        &short,
        &option,
        "options.mapper4_fixed_prg_16k needs whole 8 KiB PRG banks; bank 1 has 100 bytes",
    );
}

#[test]
fn mapper4_fixed_prg_16k_rom_hacks_continue_across_the_boundary() {
    let base = mmc3_rom(true);
    // The high byte of `lda $0390,x` ($E000) and free space at $E100.
    let mut hack = base.clone();
    hack[16 + 3 * 8192] = 0x04;
    hack[16 + 3 * 8192 + 0x100] = 0xea;
    let json = format!(r#"{{"options": {{{FIXED_PAIR}, "split_banks": true}}}}"#);
    let d = patch_round_trip(&hack, &base, &json);
    // The range of the changed byte is widened to the whole instruction,
    // in both banks.
    let diffs: Vec<_> = d
        .diffs()
        .iter()
        .map(|d| (d.bank, d.start, d.end, d.changed_bytes))
        .collect();
    assert_eq!(
        diffs,
        [
            (2, Address(0xdffe), Address(0xdfff), 0),
            (3, Address(0xe000), Address(0xe000), 1),
            (3, Address(0xe100), Address(0xe100), 1)
        ]
    );
    let names: Vec<PathBuf> = d.files().iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(names, ["out.asm", "out_bank02_03.asm"].map(PathBuf::from));
    assert_eq!(includes(d.text()), names[1..]);
    assert_lines(
        &d,
        &[
            "\t.incbin \"base.nes\", 16, 32768",
            "",
            "\t.catbank $02",
            "\t.include \"out_bank02_03.asm\"",
        ],
    );
    assert_eq!(d.text().matches(".incbin").count(), 1);
    assert_tab_indented(&d);
    let pair: Vec<&str> = d.files()[1].1.lines().skip(1).collect();
    assert_eq!(
        pair,
        [
            "\t.bank 2",
            "\t.org $DFFE",
            "\tlda $0490,x",
            "; Bank 3 starts at $E000, inside the instruction above",
            "",
            "\t.bank 3",
            "\t.org $E100",
            "\t.db $EA",
        ]
    );
    // As one file.
    let d = patch_round_trip(&hack, &base, &format!(r#"{{"options": {{{FIXED_PAIR}}}}}"#));
    assert_eq!(d.files().len(), 1);
    assert_lines(
        &d,
        &[
            "\t.incbin \"base.nes\", 16, 32768",
            "",
            "\t.catbank $02",
            "\t.bank 2",
            "\t.org $DFFE",
            "\tlda $0490,x",
            "; Bank 3 starts at $E000, inside the instruction above",
            "",
            "\t.bank 3",
            "\t.org $E100",
        ],
    );
    // Changes in the last bank only still go to the file of both.
    let mut hack = base.clone();
    hack[16 + 3 * 8192 + 0x100] = 0xea;
    let d = patch_round_trip(&hack, &base, &json);
    let names: Vec<PathBuf> = d.files().iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(names, ["out.asm", "out_bank02_03.asm"].map(PathBuf::from));
    assert_eq!(d.files()[1].1.lines().nth(1), Some("\t.bank 3"));
    assert_lines(&d, &["\t.catbank $02", "\t.include \"out_bank02_03.asm\""]);
}

#[test]
fn mapper4_fixed_prg_16k_random_code_round_trips() {
    let mut rng = Lcg(0x4d4d43);
    let code = r#""regions": [{"bank": 2, "start": "$C000", "length": 8192, "type": "code"},
                              {"bank": 3, "start": "$E000", "length": 8192, "type": "code"}]"#;
    let mut crossing = 0;
    for _ in 0..4 {
        let mut rom = vec![
            b'N', b'E', b'S', 0x1a, 2, 0, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        rom.extend((0..32768).map(|_| rng.next()));
        for split in [false, true] {
            let json =
                format!(r#"{{{code}, "options": {{{FIXED_PAIR}, "split_banks": {split}}}}}"#);
            let d = round_trip(&rom, &config(&json));
            crossing += d
                .view(Some(2), 0xdffd, 0xdfff)
                .iter()
                .filter(|l| l.address.unwrap().0 + l.size as u32 > 0xe000)
                .count();
            // A hack that changes bytes around the boundary.
            let mut hack = rom.clone();
            for offset in [0x1ffd, 0x1fff, 0x2000, 0x2002] {
                hack[16 + 2 * 8192 + offset] ^= rng.next() | 1;
            }
            patch_round_trip(&hack, &rom, &json);
        }
    }
    assert!(crossing > 0, "no instruction crossed $DFFF/$E000");
}

#[test]
fn rom_hacks_with_many_changed_chr_banks_keep_a_small_main_file() {
    // All 32 CHR banks changed: their data stays in the CHR file, so the
    // main file is far below the 1 MiB the assembler reads.
    let mut base = vec![b'N', b'E', b'S', 0x1a, 1, 32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    base.extend(vec![0; 16384 + 32 * 8192]);
    let mut hack = base.clone();
    for (i, b) in hack[16 + 16384..].iter_mut().enumerate() {
        *b = if i % 2 == 0 { 0x55 } else { 0xaa };
    }
    let d = patch_round_trip(&hack, &base, r#"{"options": {"split_banks": true}}"#);
    assert_eq!(d.files().len(), 1);
    assert!(d.text().len() < 4096, "{}", d.text().len());
    assert_eq!(d.text().matches("\"out.chr\"").count(), 1);
    assert!(
        d.text()
            .ends_with("\n\n\t.bank 2\n\t.org $0000\n\t.incbin \"out.chr\"\n"),
        "{}",
        d.text()
    );
    // The diffs still name each changed CHR bank.
    assert_eq!(d.diffs().iter().filter(|d| d.chr).count(), 32);
    assert_eq!(d.chr_file().unwrap().1, &hack[16 + 16384..]);
}

#[test]
fn mapper4_fixed_prg_16k_inline_arguments_cross_the_boundary() {
    // `jsr Print` at $DFFC is followed by 4 bytes of arguments, $DFFF-$E002.
    let rom = assemble(concat!(
        "  .inesprg 2\n  .inesmap 4\n  .bank 2\n  .org $DFF8\n",
        "Reset:\n  sei\n  cld\n  ldx #$00\n  jsr Print\n  .db 1\n",
        "  .bank 3\n  .org $E000\n  .db 2, 3, 4\nLoop:\n  jmp Loop\nPrint:\n  rts\nNmi:\n  rti\n",
        "  .org $FFFA\n  .dw Nmi, Reset, Nmi\n",
    ));
    let entry = r#""entry_points": [{"address": "$E006", "name": "Print", "inline_bytes": 4}]"#;
    let d = round_trip(&rom, &config(&format!("{{{entry}}}")));
    assert_eq!(
        issues(&d, IssueKind::CodeDataConflict),
        ["$DFFC The 4 inline bytes after the JSR at $DFFC overlap other code or data"]
    );
    let d = round_trip(
        &rom,
        &config(&format!(r#"{{{entry}, "options": {{{FIXED_PAIR}}}}}"#)),
    );
    assert!(issues(&d, IssueKind::CodeDataConflict).is_empty());
    assert_lines(
        &d,
        &[
            "\tjsr Print",
            "\t.db $01",
            "\t.bank 3",
            "\t.org $E000",
            "\t.db $02, $03, $04",
            "L_E003:",
            "\tjmp L_E003",
        ],
    );
    // The code after the arguments belongs to the routine.
    let routines = d.routines();
    let reset = routines.iter().find(|r| r.name == "RESET").unwrap();
    assert_eq!(reset.size, 10);
}

#[test]
fn mapper4_fixed_prg_16k_resolves_within_the_pair_unless_references_say() {
    // Bank 1 is also at $E000, so `jsr Init` ($E010) at $DFF3 could go to
    // bank 1 or 3.
    let rom = mmc3_rom(true);
    let banks = r#""banks": [{"bank": 1, "org": "$E000"}]"#;
    let d = round_trip(&rom, &config(&format!("{{{banks}}}")));
    assert_eq!(
        issues(&d, IssueKind::UnresolvedBank),
        ["$DFF3 $E010 is held by banks 1, 3; add a references entry with target_bank"]
    );
    // With the option, bank 2 sees bank 3 first.
    let fixed = format!(r#"{{{banks}, "options": {{{FIXED_PAIR}}}}}"#);
    let d = round_trip(&rom, &config(&fixed));
    assert!(issues(&d, IssueKind::UnresolvedBank).is_empty());
    assert_lines(&d, &["RESET:", "\tsei", "\tldx #$00", "\tjsr sub_03_E010"]);
    // references still chooses another bank.
    let chosen = fixed.replace(
        "\"options\"",
        r#""references": [{"bank": 2, "address": "$DFF3", "target_bank": 1}], "options""#,
    );
    let d = round_trip(&rom, &config(&chosen));
    assert!(issues(&d, IssueKind::UnresolvedBank).is_empty());
    assert_lines(&d, &["RESET:", "\tsei", "\tldx #$00", "\tjsr sub_01_E010"]);
}

/// A mapper 4 ROM of eight PRG banks: `banks` (sources of banks 0-5), then
/// `reset` in bank 7, followed by `Halt` and the bank switching routines
/// `SwitchSeq` (bank A at $8000, A+1 at $A000) and `Switch` ($F5 at $8000,
/// $F6 at $A000).
fn switching_rom(banks: &str, reset: &str) -> Vec<u8> {
    assemble(&format!(
        "  .inesprg 4\n  .inesmap 4\n{banks}\n  .bank 7\n  .org $E000\nReset:\n{reset}\nHalt:\n  jmp Halt\n\
         SwitchSeq:\n  sta <$F5\n  sta <$F6\n  inc <$F6\n\
         Switch:\n  lda #$06\n  sta $8000\n  lda <$F5\n  sta $8001\n  lda #$07\n  sta $8000\n  lda <$F6\n  sta $8001\n  rts\n\
         Nmi:\n  rti\n  .org $FFFA\n  .dw Nmi, Reset, Nmi\n"
    ))
}

/// Banks 0, 2 and 4 start with code at $8000 (bank 2's calls $A000), banks
/// 1, 3 and 5 at $A000.
const SWITCHED_BANKS: &str = concat!(
    "  .bank 0\n  .org $8000\n  lda #$00\n  rts\n",
    "  .bank 1\n  .org $A000\n  lda #$01\n  rts\n",
    "  .bank 2\n  .org $8000\n  lda #$02\n  jsr $A000\n  rts\n",
    "  .bank 3\n  .org $A000\n  ldx #$03\n  rts\n",
    "  .bank 4\n  .org $8000\n  lda #$04\n  rts\n",
    "  .bank 5\n  .org $A000\n  lda #$05\n  rts\n",
);

#[test]
fn returned_register_values_resolve_banks_without_guessing_at_joins() {
    let reset = concat!(
        "  lda #$02\n  jsr RestoreA\n  sta <$F5\n  jsr Switch\n  jsr $8000\n  jmp Halt\n",
        "RestoreA:\n  pha\n  lda #$04\n  pla\n  rts\n",
    );
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, reset),
        &DisasmConfig::default(),
    );
    assert_lines(&d, &["\tjsr sub_02_8000"]);
    assert!(d.text().contains("\t.db $A9, $04, $60"), "{}", d.text());

    // Two returns disagree about A: neither bank is safe to choose.
    let reset = reset.replace(
        "  pha\n  lda #$04\n  pla\n  rts",
        "  lda $0300\n  beq Two\n  lda #$04\n  rts\nTwo:\n  lda #$02\n  rts",
    );
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, &reset),
        &DisasmConfig::default(),
    );
    assert!(!issues(&d, IssueKind::UnresolvedBank).is_empty());
    assert!(!d.text().contains("sub_02_8000:"), "{}", d.text());
    assert!(!d.text().contains("sub_04_8000:"), "{}", d.text());
}

#[test]
fn stack_writes_and_call_return_bytes_do_not_preserve_old_register_values() {
    for body in [
        "  pha\n  ldx #$FC\n  lda #$04\n  sta $0100,x\n  pla\n",
        "  pha\n  ldx #$FC\n  lda #$04\n  sta $0900,x\n  pla\n",
        "  pha\n  ldx $0300\n  lda #$04\n  sta $0080,x\n  pla\n",
        "  pha\n  lda #$04\n  sta [$00],y\n  pla\n",
        "  lda $01FE\n",
    ] {
        // At SP=$FF, JSR writes $01FE/$01FF and PHA then writes $01FC.
        // The first two stores replace PHA's byte (including its RAM mirror).
        let reset = format!(
            "  ldx #$FF\n  txs\n  lda #$02\n  sta $01FE\n  jsr Restore\n  sta <$F5\n  jsr Switch\n  jsr $8000\n  jmp Halt\nRestore:\n{body}  rts\n"
        );
        let d = round_trip(
            &switching_rom(SWITCHED_BANKS, &reset),
            &DisasmConfig::default(),
        );
        assert!(!issues(&d, IssueKind::UnresolvedBank).is_empty(), "{body}");
        assert!(!d.text().contains("sub_02_8000:"), "{body}\n{}", d.text());
    }
    // A nested callee can also replace a byte its caller pushed.
    let reset = concat!(
        "  lda #$02\n  jsr Restore\n  sta <$F5\n  jsr Switch\n  jsr $8000\n  jmp Halt\n",
        "Restore:\n  pha\n  jsr Overwrite\n  pla\n  rts\n",
        "Overwrite:\n  lda #$04\n  sta $01FC\n  rts\n",
    );
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, reset),
        &DisasmConfig::default(),
    );
    assert!(!d.text().contains("sub_02_8000:"), "{}", d.text());
}

#[test]
fn summaries_follow_known_indirect_tail_jumps() {
    let reset = concat!(
        "  lda #$02\n  jsr SwitchSeq\n  jsr Tail\n  jsr $8000\n  jmp Halt\n",
        "Tail:\n  lda #LOW(Body)\n  sta <$00\n  lda #HIGH(Body)\n  sta <$01\n  jmp [$0000]\n",
        "Body:\n  rts\n",
    );
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, reset),
        &DisasmConfig::default(),
    );
    assert!(issues(&d, IssueKind::UnresolvedBank).is_empty());
    assert_lines(&d, &["\tjsr sub_02_8000", "\tjmp [$0000]"]);
    // An unknown byte prevents the summary from claiming the bank survives.
    let reset = reset.replace("lda #HIGH(Body)", "lda $0300");
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, &reset),
        &DisasmConfig::default(),
    );
    assert!(!issues(&d, IssueKind::UnresolvedBank).is_empty());
    assert!(!d.text().contains("sub_02_8000:"), "{}", d.text());
}

#[test]
fn constant_indexed_reads_follow_code_and_expand_operand_only_diffs() {
    let rom = nrom(concat!(
        "Reset:\n  ldx #$01\n  jsr KeepX\n  lda Lo,x\n  sta <$FF\n  lda Hi,x\n  sta <$00\n",
        "  ldx #$01\n  lda <$FE,x\n  sta <$02\n  lda <$FF,x\n  sta <$03\n  jmp [$0002]\n",
        "KeepX:\n  txa\n  pha\n  ldx #$07\n  pla\n  tax\n  rts\n",
        "Lo:\n  .db 0, LOW(Target)\nHi:\n  .db 0, HIGH(Target)\n",
        "Target:\n  and #$F0\n  rts\nNmi:\n  rti\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_lines(&d, &["\tand #$F0"]);
    assert!(issues(&d, IssueKind::IndirectJump).is_empty());
    // The low/high tables are read as data, never traced as instructions.
    assert!(d.text().contains("\t.db $00,"), "{}", d.text());
    let mut changed = rom.clone();
    let operand = rom
        .windows(3)
        .position(|b| b == [0x29, 0xf0, 0x60])
        .unwrap()
        + 1;
    changed[operand] = 0xfc;
    let t = Temp::new();
    fs::write(t.0.join("base.nes"), &rom).unwrap();
    let d = disasm::disassemble(
        &changed,
        Some(&rom),
        &config(r#"{"base":"base.nes"}"#),
        Path::new("out.asm"),
        &NEVER,
    )
    .unwrap();
    assert_lines(&d, &["\tand #$FC"]);
    assert_eq!(d.summary().diff.unwrap().changed_bytes, 1);
    assert!(d.verify(&t.0, Some(&t.0), &NEVER).matched);
}

#[test]
fn indirect_indexed_reads_require_known_pointers_and_indices() {
    let code = concat!(
        "Reset:\n  lda #LOW(Table)\n  sta <$FF\n  lda #HIGH(Table)\n  sta <$00\n",
        "  ldy #$01\n  jsr KeepY\n  lda [$FF],y\n  sta <$02\n  iny\n  lda [$FF],y\n  sta <$03\n",
        "  ldx #$01\n  jmp [$0002]\nKeepY:\n  rts\n",
        "Table:\n  .db 0, LOW(Target), HIGH(Target)\n",
        "Target:\n  and #$F0\n  rts\nNmi:\n  rti\n",
    );
    let d = round_trip(&nrom(code), &DisasmConfig::default());
    assert_lines(&d, &["\tand #$F0"]);
    assert!(issues(&d, IssueKind::IndirectJump).is_empty());
    // ($FE,X) with X=1 uses the same $FF/$00 pointer, then reads Table+1.
    let indexed = code
        .replace("lda #LOW(Table)", "lda #LOW(Table+1)")
        .replace("  ldy #$01", "  ldx #$01\n  ldy #$00")
        .replace("lda [$FF],y\n  sta <$02", "lda [$FE,x]\n  sta <$02");
    let d = round_trip(&nrom(&indexed), &DisasmConfig::default());
    assert_lines(&d, &["\tand #$F0"]);
    // Unknown Y must not select an entry, even though the table is in ROM.
    let unknown = code.replace("ldy #$01", "ldy $0300");
    let d = round_trip(&nrom(&unknown), &DisasmConfig::default());
    assert!(!d.text().contains("\tand #$F0"), "{}", d.text());
    assert!(!issues(&d, IssueKind::IndirectJump).is_empty());
}

#[test]
fn calls_after_a_bank_switch_go_to_the_switched_bank() {
    // Bank 2 through a routine taking A; bank 4 by writing the registers;
    // bank 0 through a routine reading $F5 and $F6.
    let rom = switching_rom(
        SWITCHED_BANKS,
        concat!(
            "  lda #$02\n  jsr SwitchSeq\n  jsr $8000\n",
            "  lda #$06\n  sta $8000\n  lda #$04\n  sta $8001\n  jsr $8000\n",
            "  lda #$00\n  sta <$F5\n  jsr Switch\n  jsr $8000\n  sta $E000\n",
        ),
    );
    let d = round_trip(&rom, &DisasmConfig::default());
    assert!(
        issues(&d, IssueKind::UnresolvedBank).is_empty(),
        "{:?}",
        d.issues()
    );
    assert_lines(
        &d,
        &[
            "\tjsr sub_E025",
            "\tjsr sub_02_8000",
            "\tsta $8000",
            "\tsta $8001",
            "\tjsr sub_04_8000",
            "\tjsr sub_E02B",
            "\tjsr sub_00_8000",
            "\tsta $E000",
        ],
    );
    // Code in bank 2 runs with bank 3 at $A000, switched in with it.
    assert_lines(
        &d,
        &[
            "sub_00_8000:",
            "sub_02_8000:",
            "\tlda #$02",
            "\tjsr sub_03_A000",
            "sub_03_A000:",
            "\tldx #$03",
            "sub_04_8000:",
        ],
    );
    // Banks 1 and 5 are never switched in: their code stays data.
    assert!(d.text().contains("\t.db $A9, $01, $60"), "{}", d.text());
    assert!(d.text().contains("\t.db $A9, $05, $60"), "{}", d.text());
    let writes = issues(&d, IssueKind::MapperWrite);
    for write in [
        "$E00A Write to ROM address $8000 at $E00A: MMC3 bank select $06: R6 (PRG bank at $8000)",
        "$E00F Write to ROM address $8001 at $E00F: MMC3 R6 = $04: the PRG bank at $8000",
        "$E01F Write to ROM address $E000 at $E01F: MMC3 IRQ disable",
    ] {
        assert!(writes.contains(&write.to_owned()), "{writes:?}");
    }
}

#[test]
fn a_bank_the_layout_places_elsewhere_is_suggested() {
    // Bank 3 is switched in at $8000, but by default it is at $A000.
    let banks = SWITCHED_BANKS.replace(".bank 3\n  .org $A000", ".bank 3\n  .org $8000");
    let rom = switching_rom(
        &banks,
        "  lda #$03\n  sta <$F5\n  jsr Switch\n  jsr $8000\n",
    );
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_eq!(
        issues(&d, IssueKind::UnresolvedBank),
        [
            "$E007 $8000 is in bank 3 here (switched in at $8000), but the layout places bank 3 at $A000; add {\"bank\": 3, \"org\": \"$8000\"} to banks"
        ]
    );
    let d = round_trip(&rom, &config(r#"{"banks": [{"bank": 3, "org": "$8000"}]}"#));
    assert!(issues(&d, IssueKind::UnresolvedBank).is_empty());
    assert_lines(&d, &["sub_03_8000:", "\tldx #$03", "\tjsr sub_03_8000"]);
}

#[test]
fn paths_that_switched_different_banks_do_not_choose_one() {
    // The two paths switch banks 2 and 4 before they join at the call.
    let reset = concat!(
        "  lda $0300\n  beq Other\n  lda #$02\n  jmp Common\n",
        "Other:\n  lda #$04\nCommon:\n  jsr SwitchSeq\n  jsr $8000\n",
    );
    let rom = switching_rom(SWITCHED_BANKS, reset);
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_eq!(
        issues(&d, IssueKind::UnresolvedBank),
        ["$E00F $8000 is held by banks 0, 2, 4; add a references entry with target_bank"]
    );
    assert_lines(&d, &["\tjsr $8000"]);
    for code in [
        "$A9, $00, $60",
        "$A9, $02, $20, $00, $A0, $60",
        "$A9, $04, $60",
    ] {
        assert!(d.text().contains(&format!("\t.db {code}")), "{}", d.text());
    }
    // Paths that switch the same bank agree.
    let rom = switching_rom(SWITCHED_BANKS, &reset.replace("#$04", "#$02"));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert!(issues(&d, IssueKind::UnresolvedBank).is_empty());
    assert_lines(&d, &["\tjsr sub_02_8000"]);
}

#[test]
fn addresses_stored_into_a_jump_pointer_are_code() {
    // RESET stores a thread entry into $93/$94; the scheduler jumps through it.
    let code = concat!(
        "Reset:\n  lda #LOW(Thread)\n  sta <$93\n  lda #HIGH(Thread)\n  sta <$94\n  jsr Run\n",
        "Halt:\n  jmp Halt\nRun:\n  jmp [$0093]\n",
        "Thread:\n  lda #$01\n  sta $0300\n  rts\nNmi:\n  rti\n",
    );
    let d = round_trip(&nrom(code), &DisasmConfig::default());
    assert_lines(&d, &["sub_E00E:", "\tjmp [$0093]", "L_E011:", "\tlda #$01"]);
    assert_eq!(
        issues(&d, IssueKind::IndirectJump),
        [
            "$E00E Indirect JMP [$0093] at $E00E: the addresses written to $0093 at $E006 are followed as code; give entry_points for its other targets"
        ]
    );
    // A pointer that code also reads data through is not a code pointer.
    let code = code.replace("Run:\n", "Run:\n  ldy #$00\n  lda [$93],y\n");
    let d = round_trip(&nrom(&code), &DisasmConfig::default());
    assert!(!d.text().contains("\tlda #$01"), "{}", d.text());
    assert!(
        d.text().contains("\t.db $A9, $01, $8D, $00, $03, $60"),
        "{}",
        d.text()
    );
}

/// A routine that jumps to the A-th address of the word table after the
/// JSR to it, through its return address.
const DISPATCH: &str = concat!(
    "Dispatch:\n  asl a\n  tay\n  iny\n  pla\n  sta <$C1\n  pla\n  sta <$C2\n",
    "  lda [$C1],y\n  pha\n  iny\n  lda [$C1],y\n  sta <$C2\n  pla\n  sta <$C1\n  jmp [$00C1]\n",
);

#[test]
fn jump_tables_after_a_dispatching_jsr_are_followed() {
    // The first handler follows the table, which ends before it.
    let rom = nrom(&format!(
        "Reset:\n  lda $0300\n  jsr Dispatch\n  .dw First, Second\n\
         First:\n  lda #$01\n  rts\nSecond:\n  lda #$02\n  rts\n{DISPATCH}Nmi:\n  rti\n"
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_lines(
        &d,
        &[
            "\tjsr sub_E010",
            "\t.dw L_E00A, L_E00D",
            "L_E00A:",
            "\tlda #$01",
            "L_E00D:",
            "\tlda #$02",
        ],
    );
    assert_eq!(
        issues(&d, IssueKind::JumpTable),
        [
            "$E006 JSR $E010 at $E003 jumps through its return address: 2 entries of a jump table were found at $E006-$E009; confirm the length with a pointers region (\"code_targets\": true)"
        ]
    );
    // A region describes the table instead.
    let d = round_trip(
        &rom,
        &config(
            r#"{"regions": [{"start": "$E006", "length": 4, "type": "pointers", "code_targets": true}]}"#,
        ),
    );
    assert!(issues(&d, IssueKind::JumpTable).is_empty());
    assert_lines(&d, &["\t.dw L_E00A, L_E00D", "L_E00A:", "\tlda #$01"]);
    // Code reached by other paths ends a table: the word after the entry
    // (the bytes of `lda #$00`, $00A9) is not an entry.
    let rom = nrom(&format!(
        "Reset:\n  lda $0300\n  beq Skip\n  jsr Dispatch\n  .dw First\nSkip:\n  lda #$00\n  rts\n\
         First:\n  lda #$01\n  rts\n{DISPATCH}Nmi:\n  rti\n"
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_lines(
        &d,
        &[
            "\t.dw L_E00D",
            "L_E00A:",
            "\tlda #$00",
            "L_E00D:",
            "\tlda #$01",
        ],
    );
    assert_eq!(
        issues(&d, IssueKind::JumpTable),
        [
            "$E008 JSR $E010 at $E005 jumps through its return address: 1 entry of a jump table was found at $E008-$E009; confirm the length with a pointers region (\"code_targets\": true)"
        ]
    );
}

#[test]
fn routines_taking_their_return_address_stop_the_trace() {
    // Print reads the two bytes after the JSR and returns past them.
    let print = concat!(
        "Print:\n  pla\n  sta <$02\n  pla\n  sta <$03\n  ldy #$01\n  lda [$02],y\n  sta $0300\n",
        "  lda <$02\n  clc\n  adc #$02\n  sta <$02\n  lda <$03\n  adc #$00\n  pha\n  lda <$02\n  pha\n  rts\n",
    );
    let rom = nrom(&format!(
        "Reset:\n  jsr Print\n  .db $A9, $07\n  jmp Reset\n{print}Nmi:\n  rti\n"
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_eq!(
        issues(&d, IssueKind::ReturnAddress),
        [
            "$E000 JSR $E008 at $E000: the routine takes its return address (arguments after the JSR?), so the code after the JSR is not followed; give the routine \"inline_bytes\" or \"no_return\" in entry_points"
        ]
    );
    assert!(
        d.text().contains("\t.db $A9, $07, $4C, $00, $E0"),
        "{}",
        d.text()
    );
    let d = round_trip(
        &rom,
        &config(r#"{"entry_points": [{"address": "$E008", "inline_bytes": 2}]}"#),
    );
    assert!(issues(&d, IssueKind::ReturnAddress).is_empty());
    assert_lines(&d, &["\tjsr sub_E008", "\t.db $A9, $07", "\tjmp RESET"]);
    // Pulling the return address only to return to the caller's caller
    // does not stop the trace of the caller.
    let rom = nrom(concat!(
        "Reset:\n  jsr Check\n  lda #$05\n  jmp Reset\n",
        "Check:\n  lda $0300\n  bne Out\n  pla\n  pla\nOut:\n  rts\nNmi:\n  rti\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert!(issues(&d, IssueKind::ReturnAddress).is_empty());
    assert_lines(&d, &["\tjsr sub_E008", "\tlda #$05"]);
}

#[test]
fn pushed_return_addresses_and_rts_jumps_are_followed() {
    // A return address pushed before a jump through a pointer that is not
    // known: whether the targets return is not known either, so the issue
    // suggests it as an entry point.
    let code = concat!(
        "Reset:\n  lda #HIGH(Back-1)\n  pha\n  lda #LOW(Back-1)\n  pha\n  jmp [$0000]\n",
        "Back:\n  lda #HIGH(Next-1)\n  pha\n  lda #LOW(Next-1)\n  pha\n  rts\n",
        "Next:\n  jmp Next\nNmi:\n  rti\n",
    );
    let d = round_trip(&nrom(code), &DisasmConfig::default());
    assert_eq!(
        issues(&d, IssueKind::IndirectJump),
        [
            "$E006 Indirect JMP [$0000] at $E006; describe its targets with a pointers region (code_targets) or entry_points (it pushes the return address $E009 first: if the targets return, give it in entry_points)"
        ]
    );
    assert!(!d.text().contains("L_E009:"), "{}", d.text());
    // Through a known pointer to a routine that returns, and an RTS to a
    // pushed constant.
    let code = code.replace(
        "Reset:\n",
        "Reset:\n  lda #LOW(Done)\n  sta <$00\n  lda #HIGH(Done)\n  sta <$01\n",
    ) + "Done:\n  rts\n";
    let d = round_trip(&nrom(&code), &DisasmConfig::default());
    assert_lines(
        &d,
        &[
            "\tjmp [$0000]",
            "L_E011:",
            "\tlda #$E0",
            "\trts",
            "L_E018:",
            "\tjmp L_E018",
        ],
    );
    // An RTS to an address pushed from tables: how to describe them.
    let rom = nrom(concat!(
        "Reset:\n  ldx $0300\n  lda Hi,x\n  pha\n  lda Lo,x\n  pha\n  rts\n",
        "A1:\n  rts\nA2:\n  rts\nLo:\n  .db LOW(A1-1), LOW(A2-1)\nHi:\n  .db HIGH(A1-1), HIGH(A2-1)\n",
        "Nmi:\n  rti\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_eq!(
        issues(&d, IssueKind::IndirectJump),
        [
            "$E00B RTS at $E00B jumps to an address pushed before it; the address comes from the low bytes at $E00E and the high bytes at $E010 indexed by X: describe them with a low_bytes region (\"pair\": \"$E010\", \"code_targets\": true, \"adjust\": -1) and a high_bytes region"
        ]
    );
}

#[test]
fn bank_tables_give_the_bank_of_each_entry() {
    // Handlers at $A000 in banks 1, 3 and 5; the table at $E018 gives the
    // bank switched in for each.
    let rom = switching_rom(
        SWITCHED_BANKS,
        concat!(
            "  ldy $0300\n  lda Banks,y\n  sta <$F6\n  jsr Switch\n",
            "  lda Lows,y\n  sta <$00\n  lda Highs,y\n  sta <$01\n  jmp [$0000]\n",
            "Banks:\n  .db 1, 3, 5\nLows:\n  .db $00, $00, $00\nHighs:\n  .db $A0, $A0, $A0\n",
        ),
    );
    let regions = r#"{"regions": [
        {"start": "$E018", "length": 3, "type": "bytes"},
        {"start": "$E01B", "length": 3, "type": "low_bytes", "pair": "$E01E", "code_targets": true, "bank_table": "$E018"},
        {"start": "$E01E", "length": 3, "type": "high_bytes"}]}"#;
    let d = round_trip(&rom, &config(regions));
    assert!(
        issues(&d, IssueKind::UnresolvedBank).is_empty(),
        "{:?}",
        d.issues()
    );
    assert_lines(
        &d,
        &[
            "\t.db LOW(L_01_A000), LOW(L_03_A000), LOW(L_05_A000)",
            "\t.db HIGH(L_01_A000), HIGH(L_03_A000), HIGH(L_05_A000)",
        ],
    );
    assert_lines(
        &d,
        &[
            "L_01_A000:",
            "\tlda #$01",
            "L_03_A000:",
            "\tldx #$03",
            "L_05_A000:",
            "\tlda #$05",
        ],
    );
    // Without it, the entries could be in any of the banks.
    let d = round_trip(
        &rom,
        &config(&regions.replace(r#", "bank_table": "$E018""#, "")),
    );
    assert_eq!(issues(&d, IssueKind::UnresolvedBank).len(), 3);
    let wrong = regions.replace(
        r#""type": "bytes"}"#,
        r#""type": "bytes", "bank_table": "$E01B"}"#,
    );
    let e = disasm::disassemble(&rom, None, &config(&wrong), Path::new("out.asm"), &NEVER)
        .err()
        .expect("bank_table on bytes should fail");
    assert_eq!(
        e[0].message,
        "regions[0] $E018: bank_table applies only to pointers and low_bytes"
    );
}

#[test]
fn rom_hacks_write_whole_instructions_reached_after_a_bank_switch() {
    // The hack changes only the operand of `lda #$02` in bank 2, which is
    // code only because the bank switch is followed.
    let base = switching_rom(SWITCHED_BANKS, "  lda #$02\n  jsr SwitchSeq\n  jsr $8000\n");
    let mut hack = base.clone();
    hack[16 + 2 * 8192 + 1] = 0x22;
    let d = patch_round_trip(&hack, &base, "{}");
    let diffs: Vec<_> = d
        .diffs()
        .iter()
        .map(|d| (d.bank, d.start, d.end, d.changed_bytes, d.code_bytes))
        .collect();
    assert_eq!(diffs, [(2, Address(0x8000), Address(0x8001), 1, 2)]);
    assert_lines(
        &d,
        &["\t.bank 2", "\t.org $8000", "sub_02_8000:", "\tlda #$22"],
    );
}

/// The issue of a call to $8000 that no known bank resolves (banks 0, 2
/// and 4 hold it), and the code of banks 2 and 4 kept as data.
fn assert_unresolved_8000(d: &Disassembly) {
    let unresolved = issues(d, IssueKind::UnresolvedBank);
    assert!(
        unresolved.iter().any(|i| i
            .ends_with("$8000 is held by banks 0, 2, 4; add a references entry with target_bank")),
        "{unresolved:?}\n{}",
        d.text()
    );
    for code in ["$A9, $02, $20, $00, $A0, $60", "$A9, $04, $60"] {
        assert!(d.text().contains(&format!("\t.db {code}")), "{}", d.text());
    }
}

#[test]
fn loops_in_routines_are_summarised_over_every_pass() {
    // The loop selects bank 2, then 4: the bank after the call is not the
    // one of the first pass.
    let reset = concat!(
        "  jsr Select\n  jsr $8000\n  jmp Halt\n",
        "Select:\n  lda #$06\n  sta $8000\n  ldx #$00\n",
        "Loop:\n  inx\n  inx\n  stx $8001\n  cpx #$04\n  bne Loop\n  rts\n",
    );
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, reset),
        &DisasmConfig::default(),
    );
    assert_unresolved_8000(&d);
}

#[test]
fn writes_to_unknown_addresses_forget_values() {
    let code = concat!(
        "Reset:\n  lda #$00\n  sta <$00\n  lda #$E1\n  sta <$01\n",
        "  ldx $0300\n  lda #$E2\n  sta <$00,x\n  jmp [$0000]\nNmi:\n  rti\n",
        "  .org $E100\n  lda #$07\n  rts\n  .org $E200\n  lda #$08\n  rts\n",
    );
    // An indexed write may have changed the pointer.
    let d = round_trip(&nrom(code), &DisasmConfig::default());
    assert!(d.text().contains("\t.db $A9, $07, $60"), "{}", d.text());
    assert!(d.text().contains("\t.db $A9, $08, $60"), "{}", d.text());
    assert_eq!(issues(&d, IssueKind::IndirectJump).len(), 1);
    // With a known index, it is the high byte.
    let d = round_trip(
        &nrom(&code.replace("ldx $0300", "ldx #$01")),
        &DisasmConfig::default(),
    );
    assert!(d.text().contains("\t.db $A9, $07, $60"), "{}", d.text());
    assert_lines(&d, &["L_E200:", "\tlda #$08"]);
    // A read-modify-write of the bank data register switches an unknown bank.
    let reset = "  lda #$06\n  sta $8000\n  lda #$02\n  sta $8001\n  inc $8001\n  jsr $8000\n";
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, reset),
        &DisasmConfig::default(),
    );
    assert_unresolved_8000(&d);
}

#[test]
fn code_that_switches_its_own_bank_out_is_not_followed_further() {
    // Bank 0 switches bank 2 in at $8000, where it runs: the bytes after
    // the switch in bank 0 are not what runs next.
    let banks = concat!(
        "  .bank 0\n  .org $8000\n  lda #$06\n  sta $8000\n  lda #$02\n  sta $8001\n",
        "  .db $A9, $33, $60\n",
        "  .bank 2\n  .org $8000\n  .db $EA, $EA, $EA, $EA, $EA, $EA, $EA, $EA\n  lda #$44\n  rts\n",
    );
    let reset = "  lda #$00\n  sta <$F5\n  jsr Switch\n  jsr $8000\n";
    let d = round_trip(&switching_rom(banks, reset), &DisasmConfig::default());
    assert_lines(&d, &["sub_00_8000:", "\tsta $8001", "\t.db $A9, $33, $60"]);
    assert!(!d.text().contains("\tlda #$44"), "{}", d.text());
}

#[test]
fn prg_mode_1_or_an_unknown_bank_select_forgets_the_bank_at_8000() {
    for select in ["  lda #$46\n  sta $8000\n", "  lda $0300\n  sta $8000\n"] {
        let reset =
            format!("  lda #$06\n  sta $8000\n  lda #$02\n  sta $8001\n{select}  jsr $8000\n");
        let d = round_trip(
            &switching_rom(SWITCHED_BANKS, &reset),
            &DisasmConfig::default(),
        );
        assert_unresolved_8000(&d);
    }
}

#[test]
fn indirect_jumps_read_the_high_byte_from_the_same_page() {
    // JMP [$00FF] reads $00FF and $0000, not $0100.
    let code = concat!(
        "Reset:\n  lda #$00\n  sta <$FF\n  lda #$E2\n  sta <$00\n  lda #$E1\n  sta $0100\n",
        "  jmp [$00FF]\nNmi:\n  rti\n",
        "  .org $E100\n  lda #$07\n  rts\n  .org $E200\n  lda #$08\n  rts\n",
    );
    let d = round_trip(&nrom(code), &DisasmConfig::default());
    assert_lines(&d, &["\tjmp [$00FF]", "L_E200:", "\tlda #$08"]);
    assert!(d.text().contains("\t.db $A9, $07, $60"), "{}", d.text());
}

#[test]
fn values_a_routine_no_longer_remembers_are_unknown() {
    // Select stores 2 into $20, then 32 other values, then selects the bank
    // in $20: the caller's 4 is not the bank.
    let mut reset = String::from(
        "  lda #$04\n  sta <$20\n  jsr Select\n  jsr $8000\n  jmp Halt\nSelect:\n  lda #$02\n  sta <$20\n",
    );
    for i in 0..32 {
        reset.push_str(&format!("  sta <${:02X}\n", 0x30 + i));
    }
    reset.push_str("  lda #$06\n  sta $8000\n  lda <$20\n  sta $8001\n  rts\n");
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, &reset),
        &DisasmConfig::default(),
    );
    assert_unresolved_8000(&d);
}

#[test]
fn a_switched_bank_the_layout_places_elsewhere_is_unresolved_even_if_one_bank_holds_the_address() {
    // Bank 1 is switched in at $8000; only bank 0 is at $8000 in the layout.
    let rom = assemble(concat!(
        "  .inesprg 2\n  .inesmap 4\n",
        "  .bank 0\n  .org $8000\n  lda #$00\n  rts\n",
        "  .bank 1\n  .org $A000\n  lda #$01\n  rts\n",
        "  .bank 3\n  .org $E000\nReset:\n  lda #$06\n  sta $8000\n  lda #$01\n  sta $8001\n",
        "  jsr $8000\nHalt:\n  jmp Halt\nNmi:\n  rti\n  .org $FFFA\n  .dw Nmi, Reset, Nmi\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_eq!(
        issues(&d, IssueKind::UnresolvedBank),
        [
            "$E00A $8000 is in bank 1 here (switched in at $8000), but the layout places bank 1 at $A000; add {\"bank\": 1, \"org\": \"$8000\"} to banks"
        ]
    );
    assert!(d.text().contains("\t.db $A9, $00, $60"), "{}", d.text());
}

#[test]
fn routines_reading_arguments_then_jumping_elsewhere_are_not_dispatchers() {
    // Args takes two bytes after the JSR, then jumps through a pointer it
    // sets itself: the bytes are no jump table, so $E110 is not code.
    let rom = nrom(concat!(
        "Reset:\n  jsr Args\n  .db $10, $E1\n  jmp Reset\n",
        "Args:\n  pla\n  sta <$02\n  pla\n  sta <$03\n  ldy #$01\n  lda [$02],y\n  sta $0300\n",
        "  lda #LOW(Post)\n  sta <$10\n  lda #HIGH(Post)\n  sta <$11\n  jmp [$0010]\n",
        "Post:\n  rts\nNmi:\n  rti\n  .org $E110\n  .db $A9, $07, $60\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert!(issues(&d, IssueKind::JumpTable).is_empty());
    assert_eq!(issues(&d, IssueKind::ReturnAddress).len(), 1);
    assert!(d.text().contains("\t.db $A9, $07, $60"), "{}", d.text());
}

#[test]
fn pointers_changed_a_byte_at_a_time_are_followed_only_when_whole() {
    // The pointer is $E100 (changed before the call), then $E210; $E110
    // is only half way.
    let rom = nrom(concat!(
        "Reset:\n  lda #$00\n  sta <$00\n  lda #$E1\n  sta <$01\n",
        "  lda #$10\n  sta <$00\n  lda #$E2\n  sta <$01\n  jsr Run\n",
        "Halt:\n  jmp Halt\nRun:\n  jmp [$0000]\nNmi:\n  rti\n",
        "  .org $E100\n  lda #$06\n  rts\n  .org $E110\n  lda #$07\n  rts\n",
        "  .org $E210\n  lda #$08\n  rts\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_lines(&d, &["L_E210:", "\tlda #$08"]);
    assert!(!d.text().contains("\tlda #$06"), "{}", d.text());
    assert!(!d.text().contains("\tlda #$07"), "{}", d.text());
}

#[test]
fn pushed_addresses_are_returns_only_from_routines_that_return() {
    // Hang never returns: the pushed $E10F+1 is no return address.
    let code = concat!(
        "Reset:\n  lda #$E1\n  pha\n  lda #$0F\n  pha\n  jmp Hang\n",
        "Hang:\n  jmp Hang\nNmi:\n  rti\n  .org $E110\n  lda #$07\n  rts\n",
    );
    let d = round_trip(&nrom(code), &DisasmConfig::default());
    assert!(d.text().contains("\t.db $A9, $07, $60"), "{}", d.text());
    // A routine that returns goes back there.
    let d = round_trip(
        &nrom(&code.replace("Hang:\n  jmp Hang", "Hang:\n  lda #$01\n  rts")),
        &DisasmConfig::default(),
    );
    assert_lines(&d, &["L_E110:", "\tlda #$07"]);
}

#[test]
fn routines_switching_banks_from_ram_can_be_annotated() {
    // Switch2 selects the bank in $F5, but may also run code that cannot be
    // followed (Sound), so the bank after a call to it is not known.
    let reset = concat!(
        "  lda #$02\n  sta <$F5\n  jsr Switch2\n  jsr $8000\n  jmp Halt\n",
        "Switch2:\n  lda #$06\n  sta $8000\n  lda <$F5\n  sta $8001\n",
        "  lda <$F8\n  beq Done\n  jsr Sound\nDone:\n  rts\nSound:\n  jmp [$0010]\n",
    );
    let rom = switching_rom(SWITCHED_BANKS, reset);
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_unresolved_8000(&d);
    assert_eq!(
        issues(&d, IssueKind::BankSwitch),
        [
            "$E00D The routine at $E00D switches in the bank in $00F5 at $8000 on the paths that can be followed, but others run code whose effects are not known, so the banks after a call to it are not known; if it always does, give it \"switch_banks\": [\"$00F5\", null] in entry_points"
        ]
    );
    let d = round_trip(
        &rom,
        &config(r#"{"entry_points": [{"address": "$E00D", "switch_banks": ["$F5", null]}]}"#),
    );
    assert!(issues(&d, IssueKind::BankSwitch).is_empty());
    // Only bank 2's call to $A000, whose bank Switch2 does not switch.
    assert_eq!(
        issues(&d, IssueKind::UnresolvedBank),
        ["$8002 $A000 is held by banks 1, 3, 5; add a references entry with target_bank"]
    );
    assert_lines(
        &d,
        &[
            "sub_02_8000:",
            "\tlda #$02",
            "\tjsr sub_E00D",
            "\tjsr sub_02_8000",
        ],
    );
    // Code that goes on into it, as SwitchSeq does into Switch, too.
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, "  lda #$02\n  jsr SwitchSeq\n  jsr $8000\n"),
        &config(r#"{"entry_points": [{"address": "$E011", "switch_banks": ["$F5", "$F6"]}]}"#),
    );
    assert!(
        issues(&d, IssueKind::UnresolvedBank).is_empty(),
        "{:?}",
        d.issues()
    );
    assert_lines(&d, &["\tjsr sub_02_8000", "sub_E011:"]);
    let e = disasm::disassemble(
        &rom,
        None,
        &config(r#"{"entry_points": [{"address": "$E00D", "switch_banks": ["$8000", null]}]}"#),
        Path::new("out.asm"),
        &NEVER,
    )
    .err()
    .expect("switch_banks outside RAM should fail");
    assert_eq!(
        e[0].message,
        "entry_points[0] $E00D: switch_banks $8000 is not RAM ($0000-$07FF, $6000-$7FFF)"
    );
}

#[test]
fn saved_stack_values_survive_a_loop_only_when_calls_can_be_followed() {
    // Restore saves $F5/$F6, loops around calls that cannot be followed
    // (pushing $10 around it), restores them and goes on into Switch.
    let source = concat!(
        "  .inesprg 4\n  .inesmap 4\n",
        "  .bank 2\n  .org $8000\n  lda #$02\n  rts\n",
        "  .bank 7\n  .org $E000\nReset:\n  lda #$02\n  sta <$F5\n  jsr Restore\n  jsr $8000\n",
        "Halt:\n  jmp Halt\n",
        "Restore:\n  lda <$F5\n  pha\n  lda <$F6\n  pha\n",
        "Loop:\n  lda <$10\n  pha\n  jsr Yield\n  pla\n  sta <$10\n  jsr Yield\n  lda <$1E\n  bne Loop\n",
        "  pla\n  sta <$F6\n  pla\n  sta <$F5\n  jmp Switch\n",
        "Yield:\n  jmp [$0010]\n",
        "Switch:\n  lda #$06\n  sta $8000\n  lda <$F5\n  sta $8001\n",
        "  lda #$07\n  sta $8000\n  lda <$F6\n  sta $8001\n  rts\n",
        "Nmi:\n  rti\n  .org $FFFA\n  .dw Nmi, Reset, Nmi\n",
    );
    let rom = assemble(source);
    let d = round_trip(
        &rom,
        &config(r#"{"entry_points": [{"address": "$E02F", "switch_banks": ["$F5", "$F6"]}]}"#),
    );
    // Yield's unknown target may write the saved stack values: it cannot
    // prove the original bank is restored when the loop finishes.
    assert!(!issues(&d, IssueKind::UnresolvedBank).is_empty());
    assert!(!d.text().contains("sub_02_8000:"), "{}", d.text());
    let known = assemble(&source.replace("Yield:\n  jmp [$0010]", "Yield:\n  rts"));
    let d = round_trip(&known, &DisasmConfig::default());
    assert!(
        issues(&d, IssueKind::UnresolvedBank).is_empty(),
        "{:?}",
        d.issues()
    );
    assert_lines(&d, &["sub_02_8000:", "\tjsr sub_E00D", "\tjsr sub_02_8000"]);
}

#[test]
fn summaries_of_routines_with_many_branches_stay_small() {
    // 32 values stored, then 4000 branches: the summary keeps one state per
    // instruction rather than the history of every path.
    let mut code = String::from("Reset:\n  jsr Many\nHalt:\n  jmp Halt\nMany:\n");
    for i in 0..32 {
        code.push_str(&format!("  sta <${:02X}\n", 0x20 + i));
    }
    for _ in 0..2000 {
        code.push_str("  bne *+2\n");
    }
    code.push_str("  rts\nNmi:\n  rti\n");
    let started = std::time::Instant::now();
    round_trip(&nrom(&code), &DisasmConfig::default());
    assert!(started.elapsed().as_secs() < 60);
}

#[test]
fn summaries_stop_where_code_switches_its_own_bank_out() {
    // Sel in bank 0 switches bank 2 in at $8000 and goes on there (an RTS
    // in bank 2); the bytes after it in bank 0 would select bank 4.
    let banks = concat!(
        "  .bank 0\n  .org $8000\n  lda #$06\n  sta $8000\n  lda #$02\n  sta $8001\n",
        "  .db $A9, $06, $8D, $00, $80, $A9, $04, $8D, $01, $80, $60\n",
        "  .bank 2\n  .org $8000\n  .db $EA, $EA, $EA, $EA, $EA, $EA, $EA, $EA, $EA, $EA\n  rts\n",
        "  .bank 4\n  .org $8000\n  lda #$04\n  rts\n",
    );
    let reset = "  lda #$00\n  sta <$F5\n  jsr Switch\n  jsr $8000\n  jsr $8000\n";
    let d = round_trip(&switching_rom(banks, reset), &DisasmConfig::default());
    assert!(d.text().contains("\t.db $A9, $04, $60"), "{}", d.text());
    assert!(
        d.text()
            .contains("\t.db $A9, $06, $8D, $00, $80, $A9, $04, $8D"),
        "{}",
        d.text()
    );
    // An unknown bank written into its own window stops the trace too.
    let banks = banks.replace("  lda #$02\n  sta $8001\n", "  lda $0300\n  sta $8001\n");
    let d = round_trip(&switching_rom(&banks, reset), &DisasmConfig::default());
    assert!(
        d.text()
            .contains("\t.db $A9, $06, $8D, $00, $80, $A9, $04, $8D"),
        "{}",
        d.text()
    );
}

#[test]
fn annotated_bank_switches_leave_the_bank_select_unknown() {
    // Switch (annotated) leaves R7 selected: the write to $8001 after it
    // does not switch bank 4 in at $8000.
    let reset = concat!(
        "  lda #$06\n  sta $8000\n  lda #$02\n  sta <$F5\n  jsr Switch\n",
        "  lda #$04\n  sta $8001\n  jsr $8000\n",
    );
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, reset),
        &config(r#"{"entry_points": [{"address": "$E01D", "switch_banks": ["$F5", "$F6"]}]}"#),
    );
    assert_unresolved_8000(&d);
}

#[test]
fn writes_through_pointers_may_switch_banks() {
    // R6 = 2, then a write through a pointer: unknown, or $8001.
    let reset = |pointer: &str| {
        format!(
            "  lda #$06\n  sta $8000\n  lda #$02\n  sta $8001\n{pointer}  ldy #$00\n  lda #$04\n  sta [$10],y\n  jsr $8000\n"
        )
    };
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, &reset("")),
        &DisasmConfig::default(),
    );
    assert_unresolved_8000(&d);
    let d = round_trip(
        &switching_rom(
            SWITCHED_BANKS,
            &reset("  lda #$01\n  sta <$10\n  lda #$80\n  sta <$11\n"),
        ),
        &DisasmConfig::default(),
    );
    assert_lines(&d, &["sub_04_8000:", "\tjsr sub_04_8000"]);
}

#[test]
fn indexed_writes_wrap_around_ffff() {
    // `sta $FFFF,x` with an unknown X may write $0000-$00FE.
    let code = concat!(
        "Reset:\n  lda #$00\n  sta <$00\n  lda #$E1\n  sta <$01\n",
        "  ldx $0300\n  lda #$E2\n  sta $FFFF,x\n  jmp [$0000]\nNmi:\n  rti\n",
        "  .org $E100\n  lda #$07\n  rts\n",
    );
    let d = round_trip(&nrom(code), &DisasmConfig::default());
    assert!(!d.text().contains("\tlda #$07"), "{}", d.text());
}

#[test]
fn only_pointers_holding_the_return_address_read_the_bytes_after_a_jsr() {
    // The return address is in $20/$21, but [$20,x] with X = 4 reads
    // through $24/$25: no jump table after the JSR.
    let rom = nrom(concat!(
        "Reset:\n  jsr Args\n  .db $10, $E1\n  jmp Reset\n",
        "Args:\n  pla\n  sta <$20\n  pla\n  sta <$21\n  ldx #$04\n  lda [$20,x]\n  sta <$10\n",
        "  lda [$20,x]\n  sta <$11\n  jmp [$0010]\nNmi:\n  rti\n",
        "  .org $E110\n  lda #$07\n  rts\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert!(issues(&d, IssueKind::JumpTable).is_empty());
    assert_eq!(issues(&d, IssueKind::ReturnAddress).len(), 1);
    assert!(!d.text().contains("\tlda #$07"), "{}", d.text());
}

#[test]
fn stored_pointers_into_switched_banks_are_not_followed() {
    // The jump through $10 comes later, when another bank may be at $8000.
    let banks = "  .bank 0\n  .org $8000\n  lda #$00\n  rts\n";
    let reset = concat!(
        "  lda #$00\n  sta <$F5\n  jsr Switch\n",
        "  lda #$00\n  sta <$10\n  lda #$80\n  sta <$11\n  jsr Run\n  jmp Halt\n",
        "Run:\n  jmp [$0010]\n",
    );
    let d = round_trip(&switching_rom(banks, reset), &DisasmConfig::default());
    assert!(d.text().contains("\t.db $A9, $00, $60"), "{}", d.text());
    // Nor reported as followed.
    let jumps = issues(&d, IssueKind::IndirectJump);
    assert_eq!(jumps.len(), 1);
    assert!(!jumps[0].contains("followed"), "{jumps:?}");
}

#[test]
fn pushed_addresses_need_a_routine_that_returns() {
    // Wrapper jumps through a pointer to Hang: it is not known to return.
    let code = concat!(
        "Reset:\n  lda #LOW(Hang)\n  sta <$10\n  lda #HIGH(Hang)\n  sta <$11\n",
        "  lda #$E1\n  pha\n  lda #$0F\n  pha\n  jmp Wrapper\n",
        "Wrapper:\n  jmp [$0010]\nHang:\n  jmp Hang\nNmi:\n  rti\n",
        "  .org $E110\n  lda #$07\n  rts\n",
    );
    let d = round_trip(&nrom(code), &DisasmConfig::default());
    assert!(!d.text().contains("\tlda #$07"), "{}", d.text());
}

#[test]
fn calls_to_routines_that_switch_the_callers_bank_end_the_trace() {
    // Sel2 writes R6 = 2 itself, then jumps where it cannot be followed:
    // the code after the JSR in bank 0 may run in another bank.
    let banks = "  .bank 0\n  .org $8000\n  jsr Sel2\n  .db $A9, $33, $60\n";
    let reset = concat!(
        "  lda #$00\n  sta <$F5\n  jsr Switch\n  jsr $8000\n  jmp Halt\n",
        "Sel2:\n  lda #$06\n  sta $8000\n  lda #$02\n  sta $8001\n  jmp [$0010]\n",
    );
    let d = round_trip(&switching_rom(banks, reset), &DisasmConfig::default());
    assert_lines(
        &d,
        &["sub_00_8000:", "\tjsr sub_E00D", "\t.db $A9, $33, $60"],
    );
}

#[test]
fn zero_page_pointers_wrap_around_ff() {
    // The pointer at $FF has its high byte at $00: [$FF],y writes $8001.
    let reset = concat!(
        "  lda #$06\n  sta $8000\n  lda #$02\n  sta $8001\n",
        "  lda #$01\n  sta <$FF\n  lda #$80\n  sta <$00\n  lda #$00\n  sta $0100\n",
        "  ldy #$00\n  lda #$04\n  sta [$FF],y\n  jsr $8000\n",
    );
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, reset),
        &DisasmConfig::default(),
    );
    assert_lines(&d, &["sub_04_8000:", "\tjsr sub_04_8000"]);
}

#[test]
fn jump_tables_after_a_jsr_need_the_return_address_unchanged() {
    // Skip moves the return address past two bytes of arguments before
    // reading the table: the table does not start right after the JSR.
    let rom = nrom(concat!(
        "Reset:\n  jsr Skip\n  .db $10, $E1\n  .dw Handler\nHandler:\n  jmp Handler\n",
        "Skip:\n  pla\n  sta <$C1\n  pla\n  sta <$C2\n  inc <$C1\n  inc <$C1\n",
        "  ldy #$01\n  lda [$C1],y\n  sta <$10\n  iny\n  lda [$C1],y\n  sta <$11\n  jmp [$0010]\n",
        "Nmi:\n  rti\n  .org $E110\n  lda #$07\n  rts\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert!(issues(&d, IssueKind::JumpTable).is_empty());
    assert_eq!(issues(&d, IssueKind::ReturnAddress).len(), 1);
    assert!(!d.text().contains("\tlda #$07"), "{}", d.text());
}

#[test]
fn switch_banks_of_unknown_ram_keep_the_bank_the_caller_runs_in() {
    // Switch is annotated. Called from bank 0, it switches in the bank in
    // $F5, which is not known here: like any call whose bank is not known,
    // it is taken to keep bank 0 at $8000 (a convention).
    let reset = "  lda #$00
  sta <$F5
  jsr Switch
  jsr $8000
";
    let annotations =
        config(r#"{"entry_points": [{"address": "$E013", "switch_banks": ["$F5", "$F6"]}]}"#);
    let banks = "  .bank 0
  .org $8000
  lda $0300
  sta <$F5
  jsr Switch
  lda #$33
  rts
";
    let d = round_trip(&switching_rom(banks, reset), &annotations);
    assert_lines(&d, &["sub_00_8000:", "	jsr sub_E013", "	lda #$33"]);
    // A bank known to be another is not.
    let banks = banks.replace("lda $0300", "lda #$02");
    let d = round_trip(&switching_rom(&banks, reset), &annotations);
    assert!(!d.text().contains("	lda #$33"), "{}", d.text());
}

#[test]
fn calls_that_switch_the_bank_and_do_not_return_end_the_trace() {
    let banks = "  .bank 0\n  .org $8000\n  jsr Sel2\n  .db $A9, $33, $60\n";
    let reset = concat!(
        "  lda #$00\n  sta <$F5\n  jsr Switch\n  jsr $8000\n  jmp Halt\n",
        "Sel2:\n  lda #$06\n  sta $8000\n  lda #$02\n  sta $8001\nHang:\n  jmp Hang\n",
    );
    let d = round_trip(&switching_rom(banks, reset), &DisasmConfig::default());
    assert_lines(
        &d,
        &["sub_00_8000:", "\tjsr sub_E00D", "\t.db $A9, $33, $60"],
    );
}

#[test]
fn routines_with_too_many_paths_keep_the_banks_they_write() {
    // Sel3 switches bank 2 in, then has more paths than a summary follows.
    let mut reset = String::from(concat!(
        "  lda #$00\n  sta <$F5\n  jsr Switch\n  jsr $8000\n  jmp Halt\n",
        "Sel3:\n  lda #$06\n  sta $8000\n  lda #$02\n  sta $8001\n",
    ));
    for _ in 0..2100 {
        reset.push_str("  bne *+2\n");
    }
    reset.push_str("  rts\n");
    let banks = "  .bank 0\n  .org $8000\n  jsr Sel3\n  .db $A9, $33, $60\n";
    let d = round_trip(&switching_rom(banks, &reset), &DisasmConfig::default());
    assert_lines(
        &d,
        &["sub_00_8000:", "\tjsr sub_E00D", "\t.db $A9, $33, $60"],
    );
}

#[test]
fn unresolved_stored_pointers_are_not_reported_as_followed() {
    // Banks 1 and 3 both hold $E100; the store is in bank 2.
    let rom = assemble(concat!(
        "  .inesprg 2\n  .inesmap 4\n  .bank 1\n  .org $E000\n  .ds 256\n  lda #$01\n  rts\n",
        "  .bank 2\n  .org $C000\nReset:\n  lda #$00\n  sta <$10\n  lda #$E1\n  sta <$11\n",
        "  jsr Run\nHalt:\n  jmp Halt\nRun:\n  jmp [$0010]\nNmi:\n  rti\n",
        "  .bank 3\n  .org $FFFA\n  .dw Nmi, Reset, Nmi\n",
    ));
    let d = round_trip(&rom, &config(r#"{"banks": [{"bank": 1, "org": "$E000"}]}"#));
    let jumps = issues(&d, IssueKind::IndirectJump);
    assert_eq!(jumps.len(), 1);
    assert!(!jumps[0].contains("followed"), "{jumps:?}");
}

#[test]
fn routines_keeping_the_banks_can_be_annotated() {
    // Yield switches bank 4 in at $8000 and back (Restore4 cannot be
    // followed): after it, bank 0's code goes on only if Yield is annotated.
    let banks = "  .bank 0\n  .org $8000\n  jsr Yield\n  lda #$33\n  rts\n";
    let reset = concat!(
        "  lda #$00\n  sta <$F5\n  jsr Switch\n  jsr $8000\n  jmp Halt\n",
        "Yield:\n  lda #$06\n  sta $8000\n  lda #$04\n  sta $8001\n  jmp [$0010]\n",
    );
    let rom = switching_rom(banks, reset);
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_lines(
        &d,
        &["sub_00_8000:", "\tjsr sub_E00D", "\t.db $A9, $33, $60"],
    );
    let d = round_trip(
        &rom,
        &config(r#"{"entry_points": [{"address": "$E00D", "keeps_banks": true}]}"#),
    );
    assert_lines(&d, &["sub_00_8000:", "\tjsr sub_E00D", "\tlda #$33"]);
    let e = disasm::disassemble(
        &rom,
        None,
        &config(r#"{"entry_points": [{"address": "$E00D", "keeps_banks": true, "switch_banks": ["$F5", null]}]}"#),
        Path::new("out.asm"),
        &NEVER,
    )
    .err()
    .expect("switch_banks with keeps_banks should fail");
    assert_eq!(
        e[0].message,
        "entry_points[0] $E00D: give switch_banks or keeps_banks, not both"
    );
}

#[test]
fn bank_writes_reach_callers_through_routines_that_do_not_return() {
    // Wrapper calls Sel2, which switches bank 2 in and never returns.
    let banks = "  .bank 0\n  .org $8000\n  jsr Wrapper\n  .db $A9, $33, $60\n";
    let reset = concat!(
        "  lda #$00\n  sta <$F5\n  jsr Switch\n  jsr $8000\n  jmp Halt\n",
        "Wrapper:\n  jsr Sel2\n  rts\n",
        "Sel2:\n  lda #$06\n  sta $8000\n  lda #$02\n  sta $8001\nHang:\n  jmp Hang\n",
    );
    let d = round_trip(&switching_rom(banks, reset), &DisasmConfig::default());
    assert!(d.text().contains("\t.db $A9, $33, $60"), "{}", d.text());
}

#[test]
fn ram_a_routine_restores_after_forgetting_it_is_kept() {
    // Keep saves $F5, writes through an unknown X (which may reach $F5),
    // and restores it: $F5 still holds 2 for Switch.
    let reset = concat!(
        "  lda #$02\n  sta <$F5\n  jsr Keep\n  jsr Switch\n  jsr $8000\n  jmp Halt\n",
        "Keep:\n  lda <$F5\n  pha\n  ldx $0300\n  sta <$00,x\n  pla\n  sta <$F5\n  rts\n",
    );
    let d = round_trip(
        &switching_rom(SWITCHED_BANKS, reset),
        &DisasmConfig::default(),
    );
    assert_lines(&d, &["\tjsr sub_02_8000"]);
}

#[test]
fn routines_going_on_into_one_keeping_the_banks_keep_them() {
    // Wrapper2 jumps to Yield, which keeps the banks (annotated).
    let banks = "  .bank 0\n  .org $8000\n  jsr Wrapper2\n  lda #$33\n  rts\n";
    let reset = concat!(
        "  lda #$00\n  sta <$F5\n  jsr Switch\n  jsr $8000\n  jmp Halt\n",
        "Wrapper2:\n  jmp Yield\n",
        "Yield:\n  lda #$06\n  sta $8000\n  lda #$04\n  sta $8001\n  jmp [$0010]\n",
    );
    let d = round_trip(
        &switching_rom(banks, reset),
        &config(r#"{"entry_points": [{"address": "$E010", "keeps_banks": true}]}"#),
    );
    assert_lines(&d, &["sub_00_8000:", "\tjsr sub_E00D", "\tlda #$33"]);
}

#[test]
fn the_last_step_a_summary_follows_counts() {
    // Sel4 switches bank 2 in with its last instructions before the budget
    // of a summary runs out.
    let mut reset =
        String::from("  lda #$00\n  sta <$F5\n  jsr Switch\n  jsr $8000\n  jmp Halt\nSel4:\n");
    for _ in 0..4092 {
        reset.push_str("  nop\n");
    }
    reset.push_str("  lda #$06\n  sta $8000\n  lda #$02\n  sta $8001\n  rts\n");
    let banks = "  .bank 0\n  .org $8000\n  jsr Sel4\n  .db $A9, $33, $60\n";
    let d = round_trip(&switching_rom(banks, &reset), &DisasmConfig::default());
    assert!(d.text().contains("\t.db $A9, $33, $60"), "{}", d.text());
}

#[test]
fn routines_moving_their_return_address_before_an_annotated_tail_take_it() {
    // Skip2 moves its return address past the two bytes after the JSR,
    // then goes on into Finish (keeps_banks): the bytes are not code.
    let rom = nrom(concat!(
        "Reset:\n  jsr Skip2\n  .db $A9, $07\n  jmp Reset\n",
        "Skip2:\n  pla\n  clc\n  adc #$02\n  tax\n  pla\n  adc #$00\n  pha\n  txa\n  pha\n  jmp Finish\n",
        "Finish:\n  rts\nNmi:\n  rti\n",
    ));
    let d = round_trip(
        &rom,
        &config(r#"{"entry_points": [{"address": "$E016", "keeps_banks": true}]}"#),
    );
    assert_eq!(issues(&d, IssueKind::ReturnAddress).len(), 1);
    assert!(!d.text().contains("\tlda #$07"), "{}", d.text());
}

#[test]
fn sha256_matches_known_digests() {
    for (data, digest) in [
        (
            &b""[..],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ] {
        assert_eq!(disasm::sha256_hex(data), digest);
    }
    assert_eq!(
        disasm::sha256_hex(&vec![b'a'; 1_000_000]),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

fn operations(json: serde_json::Value) -> Vec<disasm::Operation> {
    serde_json::from_value(json).unwrap()
}

/// Applies `ops` to `config` for `rom`, expecting success.
fn apply(rom: &[u8], config: &DisasmConfig, ops: serde_json::Value) -> DisasmConfig {
    disasm::apply_operations(config, rom, &operations(ops))
        .unwrap_or_else(|e| panic!("{e:?}"))
        .0
}

/// The errors of applying `ops` to `config` for `rom`.
fn apply_errors(rom: &[u8], config: &DisasmConfig, ops: serde_json::Value) -> Vec<String> {
    match disasm::apply_operations(config, rom, &operations(ops)) {
        Ok(_) => panic!("the operations applied"),
        Err(errors) => errors,
    }
}

#[test]
fn operations_replace_and_remove_entries_by_key() {
    let rom = nrom("Reset:\n  lda #0\n  sta $2000\nLoop:\n  jmp Loop\nNmi:\n  rti\n");
    let original = config(
        r#"{"rom": "x.nes", "labels": [{"address": "$E005", "name": "There"}, {"address": 57344, "name": "Here"}],
            "comments": [{"address": "$E000", "text": "one"}, {"address": "$E000", "text": "two", "position": "above"}]}"#,
    );
    let (changed, applied) = disasm::apply_operations(
        &original,
        &rom,
        &operations(serde_json::json!([
            {"op": "upsert", "kind": "labels", "item": {"address": "$E001", "name": "Here", "comment": "moved"}},
            {"op": "upsert", "kind": "entry_points", "item": {"address": "0xE000", "name": "Start"}},
            {"op": "remove", "kind": "comments", "key": {"address": "$E000", "position": "line"}},
            {"op": "set", "kind": "options", "item": {"address_comments": true, "data_per_line": 16}},
            {"op": "set", "kind": "options", "item": {"data_per_line": null}},
        ])),
    )
    .unwrap();
    // Entries name their bank, in the order of their addresses; values at
    // their defaults are left out.
    assert_eq!(
        disasm::canonical_json(&changed),
        concat!(
            "{\n",
            "  \"rom\": \"x.nes\",\n",
            "  \"options\": {\"address_comments\":true},\n",
            "  \"entry_points\": [\n",
            "    {\"bank\":1,\"address\":\"$E000\",\"name\":\"Start\"}\n",
            "  ],\n",
            "  \"labels\": [\n",
            "    {\"bank\":1,\"address\":\"$E001\",\"name\":\"Here\",\"comment\":\"moved\"},\n",
            "    {\"bank\":1,\"address\":\"$E005\",\"name\":\"There\"}\n",
            "  ],\n",
            "  \"comments\": [\n",
            "    {\"bank\":1,\"address\":\"$E000\",\"text\":\"two\",\"position\":\"above\"}\n",
            "  ]\n",
            "}\n",
        )
    );
    // What each operation replaced or removed, as it was.
    let previous: Vec<serde_json::Value> = applied
        .iter()
        .map(|a| serde_json::json!([a.index, a.previous]))
        .collect();
    assert_eq!(
        previous,
        [
            serde_json::json!([0, [{"bank": 1, "address": "$E000", "name": "Here"}]]),
            serde_json::json!([1, []]),
            serde_json::json!([2, [{"bank": 1, "address": "$E000", "text": "one"}]]),
            serde_json::json!([3, [{}]]),
            serde_json::json!([4, [{"address_comments": true, "data_per_line": 16}]]),
        ]
    );
    // The canonical text reads back as the same annotations.
    let again = config(&disasm::canonical_json(&changed));
    assert_eq!(
        disasm::canonical_json(&again),
        disasm::canonical_json(&changed)
    );
    round_trip(&rom, &changed);
    // Removing a comment without its position removes both.
    let changed = apply(
        &rom,
        &original,
        serde_json::json!([{"op": "remove", "kind": "comments", "key": {"bank": 1, "start": "$E000"}}]),
    );
    assert!(changed.comments.is_empty());
}

#[test]
fn operation_errors_name_the_operation() {
    let rom = nrom("Reset:\n  jmp Reset\nNmi:\n  rti\n");
    let original = config(r#"{"labels": [{"address": "$E000", "name": "Start"}]}"#);
    for (ops, message) in [
        (
            serde_json::json!([{"op": "remove", "kind": "labels", "key": {"name": "Nope"}}]),
            "operations[0] (remove labels): no entry matches the key",
        ),
        (
            serde_json::json!([{"op": "remove", "kind": "labels", "key": {"address": "$E000"}}]),
            "operations[0] (remove labels): labels are removed by name",
        ),
        (
            serde_json::json!([{"op": "remove", "kind": "banks", "key": {"address": "$E000"}}]),
            "operations[0] (remove banks): banks are removed by bank",
        ),
        (
            serde_json::json!([{"op": "upsert", "kind": "references",
                                "item": {"address": "$E000", "target_bank": 0}, "evidence": " "}]),
            "operations[0] (upsert references): this entry overrides what the analysis finds: give evidence",
        ),
        (
            serde_json::json!([{"op": "upsert", "kind": "regions", "item": {"start": "$E003"}}]),
            "operations[0] (upsert regions): give end or length",
        ),
        (
            serde_json::json!([{"op": "upsert", "kind": "regions", "item": {"start": "$E003", "length": null}}]),
            "operations[0] (upsert regions): fill in length (null)",
        ),
        (
            serde_json::json!([{"op": "upsert", "kind": "labels", "item": {"address": "$E000", "name": "X"},
                                "key": {"name": "X"}}]),
            "operations[0] (upsert labels): upsert takes item (the whole entry), not key",
        ),
        (
            serde_json::json!([{"op": "remove", "kind": "labels", "item": {}}]),
            "operations[0] (remove labels): remove takes key (the entries to remove), not item",
        ),
        (
            serde_json::json!([{"op": "upsert", "kind": "labels", "item": "Start"}]),
            "operations[0] (upsert labels): item must be an object",
        ),
        (
            serde_json::json!([{"op": "set", "kind": "labels", "item": {}}]),
            "operations[0] (set labels): set applies only to options",
        ),
        (
            serde_json::json!([{"op": "upsert", "kind": "options", "item": {}}]),
            "operations[0] (upsert options): options are changed with set",
        ),
        (
            serde_json::json!([{"op": "set", "kind": "options", "item": {"bogus": 1}}]),
            "operations[0] (set options): unknown field `bogus`",
        ),
        (
            serde_json::json!([{"op": "upsert", "kind": "labels", "item": {"address": "$1234", "name": "Ram"}}]),
            "operations[0] (upsert labels): No PRG bank holds $1234",
        ),
        (
            serde_json::json!([{"op": "upsert", "kind": "labels", "item": {"adress": "$E000", "name": "X"}}]),
            "operations[0] (upsert labels): unknown field `adress`",
        ),
    ] {
        let errors = apply_errors(&rom, &original, ops.clone());
        assert!(
            errors.len() == 1 && errors[0].starts_with(message),
            "{ops}: {errors:?}"
        );
    }
    // Every failing operation is reported, and nothing is applied.
    let errors = apply_errors(
        &rom,
        &original,
        serde_json::json!([
            {"op": "remove", "kind": "symbols", "key": {"name": "a"}},
            {"op": "upsert", "kind": "labels", "item": {"address": "$E001", "name": "Next"}},
            {"op": "remove", "kind": "symbols", "key": {"name": "b"}},
        ]),
    );
    assert_eq!(
        errors,
        [
            "operations[0] (remove symbols): no entry matches the key",
            "operations[2] (remove symbols): no entry matches the key",
        ]
    );
}

#[test]
fn duplicate_keys_are_errors_until_removed() {
    let rom = nrom("Reset:\n  jmp Reset\nNmi:\n  rti\n");
    let original = config(
        r#"{"comments": [{"address": "$E000", "text": "one"}, {"bank": 1, "address": "$E000", "text": "two"}],
            "operands": [{"address": "$E000", "expr": "RESET"}, {"address": "$E000", "expr": "Reset"}]}"#,
    );
    assert_eq!(
        apply_errors(&rom, &original, serde_json::json!([])),
        [
            "comments: 2 entries have the key bank 1 $E000 (line); remove them and give one",
            "operands: 2 entries have the key bank 1 $E000; remove them and give one",
        ]
    );
    let changed = apply(
        &rom,
        &original,
        serde_json::json!([
            {"op": "remove", "kind": "comments", "key": {"address": "$E000"}},
            {"op": "upsert", "kind": "operands", "item": {"address": "$E000", "expr": "RESET"}},
        ]),
    );
    assert!(changed.comments.is_empty());
    assert_eq!(changed.operands.len(), 1);
}

/// Four 8 KiB banks of a raw ROM: banks 0 and 1 both start with code at
/// $8000; bank 3 calls $8000.
fn two_banks_at_8000() -> Vec<u8> {
    let t = Temp::new();
    let r = t.run_with(
        concat!(
            "  .bank 0\n  .org $8000\n  rts\n",
            "  .bank 1\n  .org $8000\n  lda #1\n  rts\n",
            "  .bank 3\n  .org $E000\nReset:\n  jsr $8000\n  jmp Reset\n",
            "  .org $FFFA\n  .dw Reset, Reset, Reset\n",
        ),
        AssembleOptions {
            raw: true,
            ..AssembleOptions::default()
        },
    );
    assert!(r.success, "{:?}", r.diagnostics);
    r.binary
}

#[test]
fn banks_change_before_the_entries_that_depend_on_them() {
    let rom = two_banks_at_8000();
    // Bank 1 moves to $8000 first, so a label at $8000 needs its bank.
    let ops = serde_json::json!([
        {"op": "upsert", "kind": "labels", "item": {"address": "$8000", "name": "Here"}},
        {"op": "upsert", "kind": "banks", "item": {"bank": 1, "org": "$8000"}},
    ]);
    assert_eq!(
        apply_errors(&rom, &DisasmConfig::default(), ops),
        ["operations[0] (upsert labels): Banks 0, 1 hold $8000; give bank"]
    );
    let changed = apply(
        &rom,
        &DisasmConfig::default(),
        serde_json::json!([
            {"op": "upsert", "kind": "labels", "item": {"bank": 1, "address": "$8000", "name": "Here"}},
            {"op": "upsert", "kind": "banks", "item": {"bank": 1, "org": "$8000"}},
        ]),
    );
    assert_eq!(changed.banks[0].org, Address(0x8000));
    round_trip(&rom, &changed);
    // Without the bank's place, bank 1 is at $A000 again: the label that
    // names it no longer fits, which the analysis reports.
    let changed = apply(
        &rom,
        &changed,
        serde_json::json!([{"op": "remove", "kind": "banks", "key": {"bank": 1}}]),
    );
    let errors = disasm::disassemble(&rom, None, &changed, Path::new("out.asm"), &NEVER)
        .err()
        .unwrap();
    assert!(
        errors[0]
            .message
            .contains("Bank 1 ($A000-$BFFF) does not hold $8000"),
        "{errors:?}"
    );
}

/// The suggestions of the issues of `kind`.
fn suggestions(d: &Disassembly, kind: IssueKind) -> serde_json::Value {
    let all: Vec<_> = d
        .issues()
        .iter()
        .filter(|i| i.kind == kind)
        .map(|i| &i.suggestions)
        .collect();
    serde_json::to_value(all).unwrap()
}

/// The operations of a suggestion, with `evidence` added to each.
fn with_evidence(suggestion: &serde_json::Value) -> serde_json::Value {
    let mut ops = suggestion["operations"].clone();
    for op in ops.as_array_mut().unwrap() {
        op["evidence"] = serde_json::json!("checked in the code");
    }
    ops
}

#[test]
fn issues_suggest_references_and_bank_layouts() {
    let rom = two_banks_at_8000();
    let banks = config(r#"{"banks": [{"bank": 1, "org": "$8000"}]}"#);
    let d = round_trip(&rom, &banks);
    let found = suggestions(&d, IssueKind::UnresolvedBank);
    let reference = |bank: usize| {
        serde_json::json!({
            "description": format!("the reference is to bank {bank}"),
            "operations": [{"op": "upsert", "kind": "references",
                            "item": {"bank": 3, "address": "$E000", "target_bank": bank}}],
            "evidence_required": true,
        })
    };
    assert_eq!(found, serde_json::json!([[reference(0), reference(1)]]));
    let changed = apply(&rom, &banks, with_evidence(&found[0][1]));
    let d = round_trip(&rom, &changed);
    assert!(issues(&d, IssueKind::UnresolvedBank).is_empty());
    assert_lines(&d, &["sub_01_8000:", "\tjsr sub_01_8000"]);
    // A bank switched in where the layout does not place it.
    let layout = SWITCHED_BANKS.replace(".bank 3\n  .org $A000", ".bank 3\n  .org $8000");
    let rom = switching_rom(
        &layout,
        "  lda #$03\n  sta <$F5\n  jsr Switch\n  jsr $8000\n",
    );
    let d = round_trip(&rom, &DisasmConfig::default());
    let found = suggestions(&d, IssueKind::UnresolvedBank);
    assert_eq!(
        found,
        serde_json::json!([[{
            "description": "bank 3 is switched in at $8000",
            "operations": [{"op": "upsert", "kind": "banks", "item": {"bank": 3, "org": "$8000"}}],
            "evidence_required": false,
        }]])
    );
    // Without evidence, as it does not override what is found.
    let changed = apply(
        &rom,
        &DisasmConfig::default(),
        found[0][0]["operations"].clone(),
    );
    let d = round_trip(&rom, &changed);
    assert!(issues(&d, IssueKind::UnresolvedBank).is_empty());
}

#[test]
fn issues_suggest_entry_points_keeping_what_they_give() {
    let reset = concat!(
        "  lda #$02\n  sta <$F5\n  jsr Switch2\n  jsr $8000\n  jmp Halt\n",
        "Switch2:\n  lda #$06\n  sta $8000\n  lda <$F5\n  sta $8001\n",
        "  lda <$F8\n  beq Done\n  jsr Sound\nDone:\n  rts\nSound:\n  jmp [$0010]\n",
    );
    let rom = switching_rom(SWITCHED_BANKS, reset);
    // The name the annotations give the routine stays.
    let named = config(r#"{"entry_points": [{"address": "$E00D", "name": "Switch2"}]}"#);
    let d = round_trip(&rom, &named);
    let found = suggestions(&d, IssueKind::BankSwitch);
    assert_eq!(
        found,
        serde_json::json!([[{
            "description": "the routine always switches in the banks held in this RAM",
            "operations": [{"op": "upsert", "kind": "entry_points",
                            "item": {"bank": 7, "address": "$E00D", "name": "Switch2",
                                     "switch_banks": ["$00F5", null]}}],
            "evidence_required": true,
        }]])
    );
    assert_eq!(
        apply_errors(&rom, &named, found[0][0]["operations"].clone()),
        [
            "operations[0] (upsert entry_points): this entry overrides what the analysis finds: give evidence"
        ]
    );
    let changed = apply(&rom, &named, with_evidence(&found[0][0]));
    let d = round_trip(&rom, &changed);
    assert!(issues(&d, IssueKind::BankSwitch).is_empty());
    assert!(d.text().contains("Switch2:"), "{}", d.text());
    // A routine that takes its return address: arguments, or no return.
    let print = concat!(
        "Print:\n  pla\n  sta <$02\n  pla\n  sta <$03\n  ldy #$01\n  lda [$02],y\n  sta $0300\n",
        "  lda <$02\n  clc\n  adc #$02\n  sta <$02\n  lda <$03\n  adc #$00\n  pha\n  lda <$02\n  pha\n  rts\n",
    );
    let rom = nrom(&format!(
        "Reset:\n  jsr Print\n  .db $A9, $07\n  jmp Reset\n{print}Nmi:\n  rti\n"
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    let found = suggestions(&d, IssueKind::ReturnAddress);
    assert_eq!(
        found,
        serde_json::json!([[
            {
                "description": "the routine reads arguments after each JSR to it and returns after them",
                "operations": [{"op": "upsert", "kind": "entry_points",
                                "item": {"bank": 1, "address": "$E008", "inline_bytes": null}}],
                "needs": ["inline_bytes"],
                "evidence_required": true,
            },
            {
                "description": "the routine does not return to its caller",
                "operations": [{"op": "upsert", "kind": "entry_points",
                                "item": {"bank": 1, "address": "$E008", "no_return": true}}],
                "evidence_required": true,
            },
        ]])
    );
    // What the suggestion leaves to fill in must be filled in.
    let mut ops = with_evidence(&found[0][0]);
    assert_eq!(
        apply_errors(&rom, &DisasmConfig::default(), ops.clone()),
        ["operations[0] (upsert entry_points): fill in inline_bytes (null)"]
    );
    ops[0]["item"]["inline_bytes"] = serde_json::json!(2);
    let d = round_trip(&rom, &apply(&rom, &DisasmConfig::default(), ops));
    assert!(issues(&d, IssueKind::ReturnAddress).is_empty());
    assert_lines(&d, &["\tjsr sub_E008", "\t.db $A9, $07", "\tjmp RESET"]);
}

#[test]
fn issues_suggest_jump_tables() {
    let rom = nrom(&format!(
        "Reset:\n  lda $0300\n  jsr Dispatch\n  .dw First, Second\n\
         First:\n  lda #$01\n  rts\nSecond:\n  lda #$02\n  rts\n{DISPATCH}Nmi:\n  rti\n"
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    let found = suggestions(&d, IssueKind::JumpTable);
    assert_eq!(
        found,
        serde_json::json!([[{
            "description": "the words after the JSR are a jump table",
            "operations": [{"op": "upsert", "kind": "regions",
                            "item": {"bank": 1, "start": "$E006", "length": 4,
                                     "type": "pointers", "code_targets": true}}],
            "evidence_required": true,
        }]])
    );
    let d = round_trip(
        &rom,
        &apply(&rom, &DisasmConfig::default(), with_evidence(&found[0][0])),
    );
    assert!(issues(&d, IssueKind::JumpTable).is_empty());
    // Tables of low and high bytes read for an RTS, whose length is not known.
    let rom = nrom(concat!(
        "Reset:\n  ldx $0300\n  lda Hi,x\n  pha\n  lda Lo,x\n  pha\n  rts\n",
        "A1:\n  rts\nA2:\n  rts\nLo:\n  .db LOW(A1-1), LOW(A2-1)\nHi:\n  .db HIGH(A1-1), HIGH(A2-1)\n",
        "Nmi:\n  rti\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    let found = suggestions(&d, IssueKind::IndirectJump);
    assert_eq!(
        found,
        serde_json::json!([[{
            "description": "the jump targets come from tables of low and high bytes",
            "operations": [
                {"op": "upsert", "kind": "regions",
                 "item": {"bank": 1, "start": "$E00E", "length": null, "type": "low_bytes",
                          "pair": "$E010", "code_targets": true, "adjust": -1}},
                {"op": "upsert", "kind": "regions",
                 "item": {"bank": 1, "start": "$E010", "length": null, "type": "high_bytes"}},
            ],
            "needs": ["length"],
            "evidence_required": true,
        }]])
    );
    let mut ops = with_evidence(&found[0][0]);
    for op in ops.as_array_mut().unwrap() {
        op["item"]["length"] = serde_json::json!(2);
    }
    let d = round_trip(&rom, &apply(&rom, &DisasmConfig::default(), ops));
    // The jump stays an issue, saying that a region describes its targets,
    // and suggests nothing more.
    assert_eq!(
        issues(&d, IssueKind::IndirectJump),
        [
            "$E00B RTS at $E00B jumps to an address pushed before it; the address comes from the table at $E00E indexed by X, which a region describes"
        ]
    );
    assert_eq!(
        suggestions(&d, IssueKind::IndirectJump),
        serde_json::json!([[]])
    );
    assert_lines(&d, &["\t.db LOW(L_E00C-1), LOW(L_E00D-1)"]);
}

#[test]
fn entries_without_a_bank_that_several_banks_hold_can_be_replaced() {
    let rom = two_banks_at_8000();
    // With bank 1 at $8000, the entry point names no single bank.
    let original = config(
        r#"{"banks": [{"bank": 1, "org": "$8000"}], "entry_points": [{"address": "$8000", "name": "Here"}],
            "comments": [{"address": "$8000", "text": "one"}]}"#,
    );
    for key in [
        serde_json::json!({"bank": 1, "address": "$8000"}),
        serde_json::json!({"address": "$8000"}),
    ] {
        let changed = apply(
            &rom,
            &original,
            serde_json::json!([
                {"op": "remove", "kind": "entry_points", "key": key},
                {"op": "remove", "kind": "comments", "key": key},
            ]),
        );
        assert!(changed.entry_points.is_empty() && changed.comments.is_empty());
    }
    let changed = apply(
        &rom,
        &original,
        serde_json::json!([
            {"op": "upsert", "kind": "entry_points", "item": {"bank": 1, "address": "$8000", "name": "Here"}},
            {"op": "upsert", "kind": "comments", "item": {"bank": 1, "address": "$8000", "text": "one"}},
        ]),
    );
    assert_eq!(changed.entry_points.len(), 1);
    assert_eq!(changed.entry_points[0].bank, Some(1));
    assert_eq!(changed.comments[0].bank, Some(1));
    round_trip(&rom, &changed);
    // An entry that names its bank is not removed without it.
    assert_eq!(
        apply_errors(
            &rom,
            &changed,
            serde_json::json!([{"op": "remove", "kind": "entry_points", "key": {"address": "$8000"}}]),
        ),
        ["operations[0] (remove entry_points): no entry matches the key"]
    );
}

#[test]
fn bytes_read_from_one_table_suggest_no_address_table() {
    let rom = nrom(concat!(
        "Reset:\n  ldx $0300\n  lda Table,x\n  pha\n  pha\n  rts\n",
        "Table:\n  .db $E0, $E1\nNmi:\n  rti\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_eq!(issues(&d, IssueKind::IndirectJump).len(), 1);
    assert_eq!(
        suggestions(&d, IssueKind::IndirectJump),
        serde_json::json!([[]])
    );
}

fn ids(d: &Disassembly) -> Vec<&str> {
    d.issues().iter().map(|i| i.id.as_str()).collect()
}

#[test]
fn issue_ids_name_the_kind_and_place() {
    let rom = nrom(concat!(
        "Reset:\n  sta $8000\n  lda #0\n  beq Bad\n  rts\n",
        "Bad:\n  .db $02\nNmi:\n  rti\nLoose:\n  .db 1, 2, 3, 4, 5\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    assert_eq!(
        ids(&d),
        [
            "mapper_write:1:$E000",
            "invalid_opcode:1:$E008:from=1:$E005",
            "unknown_range:1:$E008",
            "unknown_range:1:$E00A",
        ]
    );
    // The ids stay when the annotations change what is around them.
    let d = round_trip(
        &rom,
        &config(
            r#"{"labels": [{"address": "$E008", "name": "Bad"}], "options": {"uppercase": true}}"#,
        ),
    );
    assert!(
        ids(&d).contains(&"invalid_opcode:1:$E008:from=1:$E005"),
        "{:?}",
        ids(&d)
    );
    // Header issues all name $0000; a topic tells them apart.
    let mut rom = rom;
    rom[8] = 1;
    rom.push(0);
    let d = disasm::disassemble(
        &rom,
        None,
        &DisasmConfig::default(),
        Path::new("out.asm"),
        &NEVER,
    )
    .unwrap();
    let header: Vec<&str> = ids(&d)
        .into_iter()
        .filter(|id| id.starts_with("header:"))
        .collect();
    assert_eq!(
        header,
        [
            "header:-:$0000:nes2",
            "header:-:$0000:size",
            "header:-:$0000:partial_bank",
        ]
    );
}

#[test]
fn issue_ids_name_the_path_to_the_place() {
    // Two paths enter the middle of the instruction at $E00E.
    let rom = nrom(concat!(
        "Reset:\n  jsr Two\n  lda $0300\n  beq Other\n  jmp Two+1\nOther:\n  jmp Two+1\n",
        "Two:\n  lda #$EA\n  rts\nNmi:\n  rti\n",
    ));
    let d = round_trip(&rom, &DisasmConfig::default());
    let overlaps: Vec<&str> = ids(&d)
        .into_iter()
        .filter(|id| id.starts_with("overlap:"))
        .collect();
    assert_eq!(
        overlaps,
        [
            "overlap:1:$E00F:from=1:$E008",
            "overlap:1:$E00F:from=1:$E00B"
        ],
        "{:?}",
        d.issues()
            .iter()
            .map(|i| (&i.id, &i.message))
            .collect::<Vec<_>>()
    );
    let all = ids(&d);
    let unique: std::collections::BTreeSet<&&str> = all.iter().collect();
    assert_eq!(unique.len(), all.len());
    // Without the entry point that also reaches the bad byte, the issue the
    // branch reaches keeps its id.
    let rom = nrom(concat!(
        "Reset:\n  sta $8000\n  lda #0\n  beq Bad\n  rts\n",
        "Bad:\n  .db $02\nNmi:\n  rti\n",
    ));
    let with = round_trip(&rom, &config(r#"{"entry_points": [{"address": "$E008"}]}"#));
    let without = round_trip(&rom, &DisasmConfig::default());
    let c = disasm::compare_issues(with.issues(), without.issues());
    let names =
        |issues: &[disasm::Issue]| -> Vec<String> { issues.iter().map(|i| i.id.clone()).collect() };
    assert_eq!(names(&c.resolved), ["invalid_opcode:1:$E008"]);
    assert!(c.introduced.is_empty() && c.changed.is_empty(), "{c:?}");
}

#[test]
fn narrowed_unknown_ranges_are_changed_issues() {
    let rom = nrom("Reset:\n  jmp Reset\nLoose:\n  .db 1, 2, 3, 4, 5\nNmi:\n  rti\n");
    let before = round_trip(&rom, &DisasmConfig::default());
    let after = round_trip(
        &rom,
        &config(r#"{"regions": [{"start": "$E003", "length": 1}]}"#),
    );
    let c = disasm::compare_issues(before.issues(), after.issues());
    assert!(c.resolved.is_empty() && c.introduced.is_empty(), "{c:?}");
    assert_eq!(c.changed.len(), 1);
    assert_eq!(c.changed[0].issue.id, "unknown_range:1:$E004");
    assert_eq!(c.changed[0].previous_ids, ["unknown_range:1:$E003"]);
    assert_eq!(
        c.changed[0].previous_message,
        "$E003-$E007 (5 bytes) is not reached by code nor described by a region"
    );
    let json = serde_json::to_value(&c.changed[0]).unwrap();
    assert_eq!(
        json["previous_ids"],
        serde_json::json!(["unknown_range:1:$E003"])
    );
    // A part in the middle: the first part keeps the id with another message,
    // and the second continues it; nothing is new.
    let split = round_trip(
        &rom,
        &config(r#"{"regions": [{"start": "$E004", "length": 1}]}"#),
    );
    let c = disasm::compare_issues(before.issues(), split.issues());
    assert!(c.resolved.is_empty() && c.introduced.is_empty(), "{c:?}");
    let changed: Vec<(&str, &[String])> = c
        .changed
        .iter()
        .map(|i| (i.issue.id.as_str(), i.previous_ids.as_slice()))
        .collect();
    assert_eq!(
        changed,
        [
            ("unknown_range:1:$E003", &[][..]),
            (
                "unknown_range:1:$E005",
                &["unknown_range:1:$E003".to_owned()][..]
            ),
        ]
    );
    // Without the part again, the range continues both.
    let c = disasm::compare_issues(split.issues(), before.issues());
    assert!(c.resolved.is_empty() && c.introduced.is_empty(), "{c:?}");
    assert_eq!(c.changed.len(), 1);
    assert_eq!(
        c.changed[0].previous_ids,
        ["unknown_range:1:$E003", "unknown_range:1:$E005"]
    );
    // Described whole, it is resolved.
    let all = round_trip(
        &rom,
        &config(r#"{"regions": [{"start": "$E003", "length": 5}]}"#),
    );
    let c = disasm::compare_issues(before.issues(), all.issues());
    assert_eq!(c.resolved.len(), 1);
    assert!(c.changed.is_empty());
}

#[test]
fn issues_are_compared_by_id() {
    let rom = nrom(concat!(
        "Reset:\n  ldx $0300\n  lda Hi,x\n  pha\n  lda Lo,x\n  pha\n  rts\n",
        "A1:\n  rts\nA2:\n  rts\nLo:\n  .db LOW(A1-1), LOW(A2-1)\nHi:\n  .db HIGH(A1-1), HIGH(A2-1)\n",
        "Nmi:\n  rti\n",
    ));
    let before = round_trip(&rom, &DisasmConfig::default());
    let after = round_trip(
        &rom,
        &config(
            r#"{"regions": [{"start": "$E00E", "length": 2, "type": "low_bytes", "pair": "$E010", "code_targets": true, "adjust": -1},
                            {"start": "$E010", "length": 2, "type": "high_bytes"}]}"#,
        ),
    );
    let c = disasm::compare_issues(before.issues(), after.issues());
    let names =
        |issues: &[disasm::Issue]| -> Vec<String> { issues.iter().map(|i| i.id.clone()).collect() };
    assert_eq!(names(&c.resolved), ["unknown_range:1:$E00C"]);
    assert!(c.introduced.is_empty(), "{:?}", names(&c.introduced));
    assert_eq!(c.changed.len(), 1);
    assert_eq!(c.changed[0].issue.id, "indirect_jump:1:$E00B");
    assert!(
        c.changed[0]
            .previous_message
            .contains("the low bytes at $E00E and the high bytes at $E010"),
        "{:?}",
        c.changed[0]
    );
    assert!(
        c.changed[0]
            .issue
            .message
            .contains("which a region describes")
    );
    let json = serde_json::to_value(&c.changed[0]).unwrap();
    assert_eq!(json["id"], "indirect_jump:1:$E00B");
    assert_eq!(json["kind"], "indirect_jump");
    // Nothing differs from itself.
    let same = disasm::compare_issues(after.issues(), after.issues());
    assert!(same.resolved.is_empty() && same.introduced.is_empty() && same.changed.is_empty());
}

#[test]
fn names_taken_by_automatic_names_name_their_entry() {
    let rom = nrom("Reset:\n  lda <$20\n  jmp Next\nNext:\n  jmp Reset\nNmi:\n  rti\n");
    for (json, message) in [
        (
            r#"{"labels": [{"address": "$E000", "name": "Start"}, {"address": "$E002", "name": "L_E005"}]}"#,
            "labels[1] $E002 (L_E005): Name 'L_E005' is also the automatic label of $E005 in bank 1; choose another name",
        ),
        (
            r#"{"symbols": [{"name": "zp_20", "value": "$30"}], "options": {"auto_ram_labels": true}}"#,
            "symbols[0] $0030 (zp_20): Name 'zp_20' is also an automatic RAM name; choose another name",
        ),
    ] {
        let e = disasm::disassemble(&rom, None, &config(json), Path::new("out.asm"), &NEVER)
            .err()
            .unwrap_or_else(|| panic!("{json} should fail"));
        assert!(
            e.iter().any(|d| d.message == message),
            "{json}: {:?}",
            e.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }
}

#[test]
fn canonical_order_keeps_the_order_of_names_for_one_place() {
    let rom = nrom("Reset:\n  jmp Reset\nNmi:\n  rti\n");
    for (first, second) in [("Zulu", "Alpha"), ("Alpha", "Zulu")] {
        let original = config(&format!(
            r#"{{"labels": [{{"address": "$E000", "name": "{first}"}}, {{"address": "$E000", "name": "{second}"}}],
                "symbols": [{{"name": "{first}_v", "value": "$10"}}, {{"name": "{second}_v", "value": "$10"}}]}}"#
        ));
        let changed = apply(&rom, &original, serde_json::json!([]));
        let labels: Vec<&str> = changed.labels.iter().map(|l| l.name.as_str()).collect();
        let symbols: Vec<&str> = changed.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(labels, [first, second]);
        assert_eq!(symbols, [format!("{first}_v"), format!("{second}_v")]);
    }
    // Symbols keep their order also across values, as their ranges may
    // overlap; one replaced stays in its place.
    let original = config(
        r#"{"symbols": [{"name": "Higher", "value": "$30", "size": 32}, {"name": "Lower", "value": "$20", "size": 64}]}"#,
    );
    let changed = apply(
        &rom,
        &original,
        serde_json::json!([
            {"op": "upsert", "kind": "symbols", "item": {"name": "Higher", "value": "$30", "size": 32, "comment": "first"}},
        ]),
    );
    let symbols: Vec<(&str, Option<&str>)> = changed
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s.comment.as_deref()))
        .collect();
    assert_eq!(symbols, [("Higher", Some("first")), ("Lower", None)]);
}

#[test]
fn layout_errors_of_operations_name_the_changed_banks() {
    let rom = mmc3_rom(false);
    let original = config(&format!(
        r#"{{"banks": [{{"bank": 2, "org": "$C000"}}, {{"bank": 3, "org": "$E000"}}], "options": {{{FIXED_PAIR}}}}}"#
    ));
    // The upserted bank moves to the end, then back to its place by bank.
    assert_eq!(
        apply_errors(
            &rom,
            &original,
            serde_json::json!([{"op": "upsert", "kind": "banks", "item": {"bank": 2, "org": "$8000"}}]),
        ),
        [
            "In the changed annotations: banks[0]: banks gives bank 2 org $8000, but options.mapper4_fixed_prg_16k places it at $C000"
        ]
    );
}

#[test]
fn evidence_and_status_need_format_2() {
    let rom = nrom("Reset:\n  jmp Reset\nNmi:\n  rti\n");
    let plain = apply(
        &rom,
        &config(r#"{"rom": "x.nes", "labels": [{"address": "$E000", "name": "Start"}]}"#),
        serde_json::json!([]),
    );
    assert!(!disasm::canonical_json(&plain).contains("format_version"));
    let marked = apply(
        &rom,
        &config(
            r#"{"rom": "x.nes", "labels": [{"address": "$E000", "name": "Start", "evidence": ["the reset vector"], "status": "hypothesis"}]}"#,
        ),
        serde_json::json!([]),
    );
    assert_eq!(
        disasm::canonical_json(&marked),
        concat!(
            "{\n",
            "  \"rom\": \"x.nes\",\n",
            "  \"format_version\": 2,\n",
            "  \"labels\": [\n",
            "    {\"bank\":1,\"address\":\"$E000\",\"name\":\"Start\",\"evidence\":[\"the reset vector\"],\"status\":\"hypothesis\"}\n",
            "  ]\n",
            "}\n",
        )
    );
    // Newer formats are refused with the reason.
    for version in [0, 3] {
        let json = format!(r#"{{"format_version": {version}}}"#);
        let e = disasm::disassemble(&rom, None, &config(&json), Path::new("out.asm"), &NEVER)
            .err()
            .unwrap();
        assert!(
            e[0].message.starts_with(&format!(
                "format_version {version} is not supported: this version of nesasm reads annotations up to format_version 2"
            )),
            "{e:?}"
        );
        let e = disasm::apply_operations(&config(&json), &rom, &[]).unwrap_err();
        assert!(
            e[0].starts_with(&format!("format_version {version}")),
            "{e:?}"
        );
    }
    round_trip(&rom, &config(r#"{"format_version": 1}"#));
    // Evidence has limits.
    let many: Vec<String> = (0..65).map(|i| format!("\"e{i}\"")).collect();
    let long = "x".repeat(2001);
    for (json, message) in [
        (
            format!(
                r#"{{"labels": [{{"address": "$E000", "name": "A1", "evidence": [{}]}}]}}"#,
                many.join(",")
            ),
            "labels[0]: evidence has 65 items; at most 64",
        ),
        (
            format!(r#"{{"symbols": [{{"name": "s", "value": 0, "evidence": ["{long}"]}}]}}"#),
            "symbols[0]: evidence of 2001 characters; at most 2000",
        ),
    ] {
        let e = disasm::disassemble(&rom, None, &config(&json), Path::new("out.asm"), &NEVER)
            .err()
            .unwrap();
        assert_eq!(e[0].message, message);
    }
}

#[test]
fn hypotheses_are_applied_only_when_asked() {
    let rom = nrom("Reset:\n  jmp Reset\nNmi:\n  rti\n");
    let c = config(
        r#"{"labels": [{"address": "$E000", "name": "Sure"}, {"address": "$E003", "name": "Guess", "status": "hypothesis"}]}"#,
    );
    assert_eq!(c.hypotheses(), 1);
    assert!(c.has_hypotheses());
    let d = round_trip(&rom, &c);
    assert!(d.text().contains("Sure:"), "{}", d.text());
    assert!(!d.text().contains("Guess"), "{}", d.text());
    let with = c.with_hypotheses();
    assert!(with.apply_hypotheses && with.has_hypotheses());
    assert_eq!(with.labels[1].status, disasm::Status::Hypothesis);
    let d = round_trip(&rom, &with);
    assert!(d.text().contains("Guess:"), "{}", d.text());
}

#[test]
fn operation_evidence_is_added_to_the_entry() {
    let rom = two_banks_at_8000();
    let banks = config(r#"{"banks": [{"bank": 1, "org": "$8000"}]}"#);
    let reference = |evidence: serde_json::Value, op: Option<&str>| {
        let mut operation = serde_json::json!({"op": "upsert", "kind": "references",
            "item": {"bank": 3, "address": "$E000", "target_bank": 1, "evidence": evidence}});
        if let Some(op) = op {
            operation["evidence"] = serde_json::json!(op);
        }
        serde_json::json!([operation])
    };
    let evidence = |c: &DisasmConfig| c.references[0].evidence.clone();
    // Added after what the item gives, once.
    let c = apply(
        &rom,
        &banks,
        reference(
            serde_json::json!(["A=1 before"]),
            Some("restored on return"),
        ),
    );
    assert_eq!(evidence(&c), ["A=1 before", "restored on return"]);
    let c = apply(
        &rom,
        &banks,
        reference(serde_json::json!(["A=1 before"]), Some("A=1 before")),
    );
    assert_eq!(evidence(&c), ["A=1 before"]);
    // Evidence the item gives is enough.
    let c = apply(
        &rom,
        &banks,
        reference(serde_json::json!(["A=1 before"]), None),
    );
    assert_eq!(evidence(&c), ["A=1 before"]);
    assert_eq!(
        apply_errors(&rom, &banks, reference(serde_json::json!([]), None)),
        [
            "operations[0] (upsert references): this entry overrides what the analysis finds: give evidence"
        ]
    );
    // A hypothesis needs a reason too, of any kind.
    let guess = serde_json::json!([{"op": "upsert", "kind": "labels",
        "item": {"bank": 3, "address": "$E000", "name": "Maybe", "status": "hypothesis"}}]);
    assert_eq!(
        apply_errors(&rom, &banks, guess.clone()),
        [
            "operations[0] (upsert labels): this entry overrides what the analysis finds: give evidence"
        ]
    );
    let mut guess = guess;
    guess[0]["evidence"] = serde_json::json!("looks like a loop");
    let c = apply(&rom, &banks, guess);
    assert_eq!(c.labels[0].status, disasm::Status::Hypothesis);
    assert_eq!(c.labels[0].evidence, ["looks like a loop"]);
}

#[test]
fn hypotheses_keep_their_place_and_status() {
    let rom = nrom("Reset:\n  jmp Reset\nNmi:\n  rti\n");
    // Errors name the entry by its index in the annotations as given.
    let c = config(
        r#"{"labels": [{"address": "$E000", "name": "Guess", "status": "hypothesis"}, {"address": "$E003", "name": "9z"}]}"#,
    );
    let e = disasm::disassemble(&rom, None, &c, Path::new("out.asm"), &NEVER)
        .err()
        .unwrap();
    assert!(
        e[0].message
            .starts_with("labels[1] $E003 (9z): Invalid name '9z'"),
        "{e:?}"
    );
    // Suggestions made with hypotheses applied keep their status.
    let print = concat!(
        "Print:\n  pla\n  sta <$02\n  pla\n  sta <$03\n  ldy #$01\n  lda [$02],y\n  sta $0300\n",
        "  lda <$02\n  clc\n  adc #$02\n  sta <$02\n  lda <$03\n  adc #$00\n  pha\n  lda <$02\n  pha\n  rts\n",
    );
    let rom = nrom(&format!(
        "Reset:\n  jsr Print\n  .db $A9, $07\n  jmp Reset\n{print}Nmi:\n  rti\n"
    ));
    let c = config(
        r#"{"entry_points": [{"address": "$E008", "name": "Print", "status": "hypothesis", "evidence": ["called from RESET"]}]}"#,
    );
    let d = round_trip(&rom, &c.with_hypotheses());
    let found = suggestions(&d, IssueKind::ReturnAddress);
    assert_eq!(
        found[0][1]["operations"][0]["item"],
        serde_json::json!({"bank": 1, "address": "$E008", "name": "Print", "no_return": true,
                           "status": "hypothesis", "evidence": ["called from RESET"]})
    );
}

#[test]
fn evidence_added_by_operations_is_within_the_limits() {
    let rom = two_banks_at_8000();
    let banks = config(r#"{"banks": [{"bank": 1, "org": "$8000"}]}"#);
    let evidence: Vec<String> = (0..64).map(|i| format!("e{i}")).collect();
    let ops = serde_json::json!([{"op": "upsert", "kind": "references",
        "item": {"bank": 3, "address": "$E000", "target_bank": 1, "evidence": evidence},
        "evidence": "one more"}]);
    assert_eq!(
        apply_errors(&rom, &banks, ops),
        ["In the changed annotations: references[0]: evidence has 65 items; at most 64"]
    );
}

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
            "$C012 Indirect JMP [$0010] at $C012; describe its targets with a pointers region (code_targets) or entry_points"
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
            "Invalid name '9lives'",
        ),
        (
            r#"{"labels": [{"address": "$E000", "name": "x"}]}"#,
            "Name 'x' is reserved by the assembler",
        ),
        (
            r#"{"labels": [{"address": "$E000", "name": "a1"}], "symbols": [{"name": "a1", "value": 0}]}"#,
            "Name 'a1' is used by label $E000 and symbol $0000",
        ),
        (
            r#"{"labels": [{"address": "$1234", "name": "a1"}]}"#,
            "labels $1234 (a1): No PRG bank holds $1234",
        ),
        (
            r#"{"labels": [{"bank": 0, "address": "$E000", "name": "a1"}]}"#,
            "labels $E000 (a1): Bank 0 ($C000-$DFFF) does not hold $E000",
        ),
        (
            r#"{"regions": [{"start": "$E000", "length": 4}, {"start": "$E002", "length": 4}]}"#,
            "Regions at $E000 and $E002 in bank 1 overlap",
        ),
        (
            r#"{"regions": [{"start": "$E000", "length": 2, "type": "low_bytes"}]}"#,
            "regions $E000: low_bytes needs pair (the high_bytes table)",
        ),
        (
            r#"{"regions": [{"start": "$FFFF", "length": 2}]}"#,
            "regions $FFFF: the range runs past the end of bank 1 ($FFFF)",
        ),
        (
            r#"{"banks": [{"bank": 0, "org": "$9000"}]}"#,
            "Bank 0 org $9000 must be a multiple of $2000 up to $E000",
        ),
        (
            r#"{"load_address": "$8000"}"#,
            "load_address applies only to raw .bin files",
        ),
        (
            r#"{"operands": [{"address": "$E000", "expr": " "}]}"#,
            "operands $E000: the expression must be one line without a comment",
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
    // The base's PRG banks, then its CHR bank after the new PRG banks.
    assert_lines(
        &d,
        &[
            "\t.bank 0",
            "\t.org $8000",
            "\t.incbin \"base.nes\", 16, 16384",
            "\t.bank 4",
            "\t.org $0000",
            "\t.incbin \"base.nes\", 16400, 8192",
            "\t.bank 2",
            "\t.org $C000",
        ],
    );
    assert_eq!(d.text().matches("\"base.nes\"").count(), 2);
    assert!(d.chr_file().is_none());
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
    // Bank 0 is unchanged and has no file; the CHR bank 4 has none either.
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
    // The main file includes the base once per run of banks it holds in order.
    assert_lines(
        &d,
        &[
            "\t.inesprg 2",
            "\t.bank 0",
            "\t.org $C000",
            "\t.incbin \"base.nes\", 16, 16384",
            "\t.bank 4",
            "\t.org $0000",
            "\t.incbin \"base.nes\", 16400, 8192",
            "",
            "\t.include \"out_bank01.asm\"",
            "\t.include \"out_bank03.asm\"",
            // The changed CHR bank is written over the base in the main file.
            "",
            "\t.bank 4",
            "\t.org $0000",
            "\t.incbin \"out.chr\", 0, 8192",
        ],
    );
    assert!(
        d.text()
            .ends_with("\t.include \"out_bank03.asm\"\n\n\t.bank 4\n\t.org $0000\n\t.incbin \"out.chr\", 0, 8192\n"),
        "{}",
        d.text()
    );
    assert_eq!(d.text().matches("\"base.nes\"").count(), 2);
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
    // New banks are written whole; the changed CHR bank comes from the CHR file.
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
    // As one file, and with CHR data as `.db`.
    let d = patch_round_trip(
        &hack,
        &base,
        &json.replace(r#""split_banks": true"#, r#""chr": "db""#),
    );
    assert_eq!(d.files().len(), 1);
    assert_tab_indented(&d);
    assert_lines(
        &d,
        &[
            "\t.bank 4",
            "\t.org $0000",
            "\t.db $00, $01, $02, $03, $04, $05, $06, $07",
        ],
    );
    // Split, with CHR data as `.db`: the data is in the main file too.
    let d = patch_round_trip(
        &hack,
        &base,
        &json.replace(
            r#""split_banks": true"#,
            r#""split_banks": true, "chr": "db""#,
        ),
    );
    assert_eq!(includes(d.text()), names[1..]);
    assert!(d.chr_file().is_none());
    assert_tab_indented(&d);
    assert_lines(
        &d,
        &[
            "\t.include \"out_bank03.asm\"",
            "",
            "\t.bank 4",
            "\t.org $0000",
            "\t.db $00, $01, $02, $03, $04, $05, $06, $07",
        ],
    );
    for (path, text) in &d.files()[1..] {
        assert!(!text.contains("\t.bank 4"), "{}: {text}", path.display());
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
        "patches $1000: No PRG bank holds $1000",
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
            "regions $E000: the range runs past the end of bank 1 ($FFFF)",
        ),
        (
            r#"{"entry_points": [{"address": "$E004", "inline_bytes": 18446744073709551615}]}"#,
            "entry_points $E004: inline_bytes must be at most 8192",
        ),
        (
            r#"{"symbols": [{"name": "buf", "value": 1, "size": 4294967295}]}"#,
            "Symbol buf covers addresses past $FFFF",
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
        "banks gives bank 2 org $8000, but options.mapper4_fixed_prg_16k places it at $C000",
    );
    fails(
        &rom,
        &banks(r#"{"bank": 3, "org": "$A000"}"#),
        "banks gives bank 3 org $A000, but options.mapper4_fixed_prg_16k places it at $E000",
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
        let mut rom = vec![b'N', b'E', b'S', 0x1a, 2, 0, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0];
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

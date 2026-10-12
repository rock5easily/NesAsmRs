mod common;

use common::Temp;
use nesasm_core::disasm::{self, Address, DisasmConfig, Disassembly, IssueKind};
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
        "  .inesprg 1",
        "  .inesmir 1",
        "PPUCTRL         = $2000",
        "RESET:",
        "  sei",
        "  ldx #$FF",
        "  jsr sub_C011",
        "L_C007:",
        "  lda <$10",
        "  sta $0300,x",
        "  bne L_C007",
        "sub_C011:",
        "  lda D_C018,x",
        "  sta PPUCTRL",
        "D_C018:",
        "  .db $01, $02, $03, $04",
        "NMI:",
        "  rti",
        "  .dw NMI, RESET, NMI",
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
            "  jsr Print",
            "  .db $AA, $BB",
            "  ldx buffer+1 ; read the count",
            "  lda D_C019,x",
            "  sta <ptr",
            "  lda D_C01B,x",
            "  sta <ptr+1",
            "  jmp [ptr]",
            "; Prints the text after the JSR",
            "Print:",
            "  rts",
            "L_C016:",
            "  jmp L_C016",
            "D_C019:",
            "  .db LOW(L_C016), LOW(Print)",
            "D_C01B:",
            "  .db HIGH(L_C016), HIGH(Print)",
            "  .dw L_C016-1",
            "; Message",
            "  .db \"HI!\", $00",
            "NMI:",
            "  rti",
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
            "  lda $0012",
            "  lda <$12",
            "  sta $0012,y",
            "  ldx <$12,y",
            "  lda [$12],y",
            "  sta [$12,x]",
        ],
    );
    let c = config(
        r#"{"symbols": [{"name": "zvar", "value": "$12"}], "options": {"uppercase": true}}"#,
    );
    let d = round_trip(&rom, &c);
    assert_lines(
        &d,
        &[
            "  LDA zvar",
            "  LDA <zvar",
            "  STA zvar,Y",
            "  LDX <zvar,Y",
            "  LDA [zvar],Y",
            "  STA [zvar,X]",
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
            "  bit $02A9",
            "  jsr sub_E000",
            "  jsr L_E002+1",
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
            "  bit $02A9",
            "Second = L_E002+1",
            "  jsr Second",
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
            "  .bank 3",
            "  jsr $8000",
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
            "  .bank 1",
            "  .org $8000",
            "sub_01_8000:",
            "  lda #$01",
            "  jsr sub_01_8000",
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
            "  .bank 0",
            "  .org $0300",
            "Start:",
            "  lda #$00",
            "  sta PPUCTRL",
            "  rts",
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
            "  .ineschr 1",
            "  .bank 2",
            "  .incbin \"out.chr\", 0, 8192",
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
        &["  .bank 2", "  .org $0000", "  .db $00, $00, $00, $01"],
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
            "  sta $8000",
            "L_E008:",
            "  .db $02",
            "NMI:",
            "  .db $01, $02, $03, $04, $05",
            "  .ds 8171",
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
    assert_eq!(view, ["sub_E00C:", "  lda JOY1", "  inc <count", "  rts"]);
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
            "  .inesmir 0",
            "",
            "  .include \"out_bank00.asm\"",
            "  .include \"out_bank01.asm\"",
        ],
    );
    let bank1 = &d.files()[2].1;
    assert!(bank1.lines().nth(1) == Some("  .bank 1"), "{bank1}");
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
            "; Changed ranges are disassembled; the rest is included from base.nes.",
            "; Labels in the unchanged ranges",
            "L_E00B          = $E00B",
            "  .bank 0",
            "  .org $C000",
            "  .incbin \"base.nes\", 16, 8192",
            "  .bank 1",
            "  .org $E000",
            "  .incbin \"base.nes\", 8208, 5",
            "  jsr sub_E100",
            "  .incbin \"base.nes\", 8216, 248",
            "sub_E100:",
            "  lda #$02",
            "  sta $0300",
            "  jmp L_E00B",
            "  .incbin \"base.nes\", 8472, 7928",
        ],
    );
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
            "  .incbin \"base.nes\", 8208, 2",
            "; Hooked start-up",
            "  sta PPUCTRL",
            "  jsr sub_E100",
            "  .incbin \"base.nes\", 8216, 3",
            "  lda #$01",
            "  rts",
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
    assert_lines(&d, &["  .bank 4", "  .incbin \"base.nes\", 16400, 8192"]);
    assert!(d.chr_file().is_none());
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
    assert_lines(&d, &["  .inesmir 4"]);
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
                .contains("contains a quote or control character"),
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
    assert_lines(&d, &["PPUMASK         = $2001", "  sta PPUMASK"]);
    // In a ROM hack, a label of an unchanged range named only by an override.
    let (base, hack) = hack_roms();
    let d = patch_round_trip(
        &hack,
        &base,
        r#"{"labels": [{"address": "$E00E", "name": "Handler"}],
            "operands": [{"address": "$E105", "expr": "Handler-3"}]}"#,
    );
    assert_lines(&d, &["Handler         = $E00E", "  jmp Handler-3"]);
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
    assert_lines(&d, &["  .dw $0201", "  .db $01", "  .db $02"]);
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

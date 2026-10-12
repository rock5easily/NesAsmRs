mod common;

use common::Temp;
use nesasm_core::inspect::{InesHeader, Mirroring, Rom};
use nesasm_core::{
    ArtifactKind, AssembleOptions, AssembleResult, BankRef, DiagnosticCode, ListLevel,
    ReferenceTopic, Severity, SourceEncoding,
};
use std::{fs, path::Path, sync::atomic::AtomicBool};

#[test]
fn list_level_clamps_and_serializes_as_a_number() {
    for (level, expected) in [
        (i64::MIN, ListLevel::Off),
        (0, ListLevel::Off),
        (1, ListLevel::Brief),
        (2, ListLevel::Normal),
        (3, ListLevel::Full),
        (99, ListLevel::Full),
    ] {
        assert_eq!(ListLevel::clamped(level), expected, "{level}");
    }
    for (level, json) in [
        (ListLevel::Off, "0"),
        (ListLevel::Brief, "1"),
        (ListLevel::Normal, "2"),
        (ListLevel::Full, "3"),
    ] {
        assert_eq!(serde_json::to_string(&level).unwrap(), json);
        assert_eq!(serde_json::from_str::<ListLevel>(json).unwrap(), level);
    }
}

#[test]
fn bank_ref_uses_the_legacy_numbers() {
    for (bank, number) in [
        (BankRef::Rom(0), 0),
        (BankRef::Rom(127), 127),
        (BankRef::Constant, 240),
        (BankRef::Procedure, 241),
    ] {
        assert_eq!(bank.number(), number);
        assert_eq!(serde_json::to_string(&bank).unwrap(), number.to_string());
        assert_eq!(
            serde_json::from_str::<BankRef>(&number.to_string()).unwrap(),
            bank
        );
    }
    assert_eq!(BankRef::Rom(5).rom(), Some(5));
    assert_eq!(BankRef::Constant.rom(), None);
    for invalid in ["128", "239", "242", "-1"] {
        assert!(
            serde_json::from_str::<BankRef>(invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn diagnostic_codes_and_severity_keep_their_json_names() {
    use DiagnosticCode::*;
    for (code, name) in [
        (Io, "E_IO"),
        (Assembly, "E_ASSEMBLY"),
        (Symbol, "E_SYMBOL"),
        (Macro, "E_MACRO"),
        (Conditional, "E_CONDITIONAL"),
        (Include, "E_INCLUDE"),
        (Procedure, "E_PROC"),
        (Limit, "E_LIMIT"),
        (Cancelled, "E_CANCELLED"),
        (Output, "E_OUTPUT"),
        (Timeout, "E_TIMEOUT"),
        (Internal, "E_INTERNAL"),
        (BankOverflow, "W_BANK_OVERFLOW"),
        (UndefinedIf, "W_UNDEFINED_IF"),
    ] {
        assert_eq!(code.as_str(), name);
        assert_eq!(code.to_string(), name);
        assert_eq!(serde_json::to_string(&code).unwrap(), format!("\"{name}\""));
        assert_eq!(
            serde_json::from_str::<DiagnosticCode>(&format!("\"{name}\"")).unwrap(),
            code
        );
    }
    for (severity, name) in [(Severity::Error, "error"), (Severity::Warning, "warning")] {
        assert_eq!(severity.to_string(), name);
        assert_eq!(
            serde_json::to_string(&severity).unwrap(),
            format!("\"{name}\"")
        );
    }
}

#[test]
fn encode_source_for_in_memory_files() {
    assert_eq!(
        nesasm_core::encode_source("a あ", &SourceEncoding::Utf8).unwrap(),
        "a あ".as_bytes()
    );
    assert_eq!(
        nesasm_core::encode_source("a あ", &SourceEncoding::Sjis).unwrap(),
        [b'a', b' ', 0x82, 0xa0]
    );
    assert!(nesasm_core::encode_source("\u{1F600}", &SourceEncoding::Sjis).is_err());
}

#[test]
fn reference_topics_have_unique_names() {
    let mut names = Vec::new();
    for topic in ReferenceTopic::ALL {
        assert!(!topic.summary().is_empty());
        assert_eq!(
            nesasm_core::reference(Some(topic.name())).unwrap(),
            topic.text()
        );
        assert_eq!(
            nesasm_core::reference(Some(&topic.name().to_uppercase())).unwrap(),
            topic.text()
        );
        names.push(topic.name());
    }
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), ReferenceTopic::ALL.len());
    assert_eq!(
        nesasm_core::reference(None).unwrap(),
        ReferenceTopic::Index.text()
    );
    assert!(nesasm_core::reference(Some("bogus")).is_err());
}

#[test]
fn build_writes_artifacts_or_reports_output_errors() {
    let t = Temp::new();
    t.write("test.asm", "  .list\n  .db 1\n");
    let cancel = AtomicBool::new(false);
    let request = t.request(AssembleOptions::default());
    let (r, artifacts) = nesasm_core::build(&request, None, &cancel);
    assert!(r.success, "{:?}", r.diagnostics);
    let kinds: Vec<_> = artifacts.iter().map(|a| a.kind).collect();
    assert_eq!(kinds, [ArtifactKind::Rom, ArtifactKind::Listing]);
    assert_eq!(artifacts[0].size, 16 + 8192);
    assert_eq!(fs::read(t.0.join("test.nes")).unwrap().len(), 16 + 8192);
    assert!(t.0.join("test.lst").is_file());
    // Text artifacts use the platform line ending.
    let listing = fs::read_to_string(t.0.join("test.lst")).unwrap();
    assert_eq!(listing.contains("\r\n"), cfg!(windows));
    // An output outside the root is an E_OUTPUT error, and nothing is written.
    let (r, artifacts) = nesasm_core::build(&request, Some(Path::new("../out.nes")), &cancel);
    assert!(!r.success);
    assert!(artifacts.is_empty());
    assert_eq!(r.diagnostics.last().unwrap().code, DiagnosticCode::Output);
    // A failed assembly writes nothing.
    t.write("test.asm", "  lda #300\n");
    fs::remove_file(t.0.join("test.nes")).unwrap();
    let (r, artifacts) = nesasm_core::build(&request, None, &cancel);
    assert!(!r.success && artifacts.is_empty());
    assert!(!t.0.join("test.nes").exists());
}

#[test]
fn write_artifacts_rejects_failed_and_conflicting_results() {
    let t = Temp::new();
    let write = |result: &AssembleResult, output: &str, options: &AssembleOptions| {
        nesasm_core::write_artifacts(
            result,
            Path::new("test.asm"),
            Some(Path::new(output)),
            &t.0,
            None,
            options,
        )
    };
    let options = AssembleOptions::default();
    let failed = AssembleResult::default();
    assert_eq!(
        write(&failed, "a.nes", &options).unwrap_err(),
        "Cannot write artifacts for a failed assembly"
    );
    let listed = AssembleResult {
        success: true,
        listing: Some("listing\n".into()),
        ..AssembleResult::default()
    };
    assert_eq!(
        write(&listed, "a.lst", &options).unwrap_err(),
        "Artifact output paths collide"
    );
    let sjis = AssembleOptions {
        encoding: SourceEncoding::Sjis,
        ..AssembleOptions::default()
    };
    let emoji = AssembleResult {
        success: true,
        listing: Some("\u{1F600}\n".into()),
        ..AssembleResult::default()
    };
    assert_eq!(
        write(&emoji, "b.nes", &sjis).unwrap_err(),
        "Listing cannot be encoded as SJIS"
    );
    assert!(!t.0.join("b.nes").exists());
}

#[test]
fn srec_output_replaces_the_rom_file() {
    let t = Temp::new();
    let options = AssembleOptions {
        srec: true,
        ..AssembleOptions::default()
    };
    t.write("test.asm", "  .db 1\n");
    let request = t.request(options);
    let (r, artifacts) = nesasm_core::build(&request, None, &AtomicBool::new(false));
    assert!(r.success, "{:?}", r.diagnostics);
    let kinds: Vec<_> = artifacts.iter().map(|a| a.kind).collect();
    assert_eq!(kinds, [ArtifactKind::Srec]);
    assert!(t.0.join("test.s28").is_file());
    assert!(!t.0.join("test.nes").exists());
}

fn header(flags6: u8, flags7: u8) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[..4].copy_from_slice(b"NES\x1a");
    h[4] = 1;
    h[6] = flags6;
    h[7] = flags7;
    h
}

#[test]
fn ines_header_flags() {
    assert!(InesHeader::parse(b"NES").is_err());
    assert!(InesHeader::parse(&[0; 16]).is_err());
    let h = InesHeader::parse(&header(0x00, 0x00)).unwrap();
    assert_eq!(
        (h.mirroring, h.battery, h.trainer, h.nes2),
        (Mirroring::Horizontal, false, false, false)
    );
    let h = InesHeader::parse(&header(0x0f | 0x30, 0x48)).unwrap();
    assert_eq!(
        (h.mirroring, h.battery, h.trainer, h.nes2, h.mapper),
        (Mirroring::FourScreen, true, true, true, 0x43)
    );
}

#[test]
fn rom_files_with_trainers() {
    let mut file = header(0x04, 0).to_vec();
    assert_eq!(
        Rom::from_file(&file).unwrap_err(),
        "File ends inside the trainer"
    );
    file.extend([0xff; 512]);
    file.extend([0xea; 16384]);
    let rom = Rom::from_file(&file).unwrap();
    assert_eq!(rom.payload.len(), 16384);
    assert_eq!(rom.payload[0], 0xea);
}

#[test]
fn rom_warnings() {
    // A raw payload: the vectors come from the last bank.
    let mut raw = vec![0xea; 0x4000];
    raw[0x3ffc..0x3ffe].copy_from_slice(&[0x00, 0x60]);
    let rom = Rom::from_file(&raw).unwrap();
    assert_eq!(rom.vectors()[0].bank, 1);
    let warnings = rom.warnings();
    assert!(warnings.iter().any(|w| w == "No iNES header (raw payload)"));
    assert!(
        warnings
            .iter()
            .any(|w| w == "The RESET vector in bank 1 is $6000, outside PRG ROM"),
        "{warnings:?}"
    );
    // A header without PRG ROM and an empty payload.
    let mut file = header(0, 0).to_vec();
    file[4] = 0;
    let warnings = Rom::from_file(&file).unwrap().warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w == "The header declares no PRG ROM (.inesprg)"),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w == "No bank holds the interrupt vectors at $FFFA"),
        "{warnings:?}"
    );
}

#[test]
fn disassembly_names_branches_and_symbols_in_other_banks() {
    let t = Temp::new();
    let r = t.run(concat!(
        "  .bank 0\n  .org $8000\nLoop:\n  bne Loop\n  jsr Far\n",
        "  .bank 2\n  .org $9000\nFar:\n  rts\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    let rom = Rom::from_assembly(&r);
    let code = rom.disassemble(0, 0x8000, 2, &r.symbols).unwrap();
    assert_eq!(code[0].instruction, "BNE $8000");
    assert_eq!(code[0].operand_symbols, ["Loop"]);
    assert_eq!(code[1].instruction, "JSR $9000");
    assert_eq!(code[1].operand_symbols, ["Far"]);
    // An instruction cut off at the end of the data is shown as a byte.
    let cut = Rom::from_file(&[0xad, 0x00]).unwrap();
    let line = &cut.disassemble(0, 0x8000, 2, &r.symbols).unwrap();
    assert_eq!(
        line.iter()
            .map(|l| l.instruction.as_str())
            .collect::<Vec<_>>(),
        [".db $AD", "BRK"]
    );
    let dump = rom.hexdump(0, 0x8000, 20).unwrap();
    assert_eq!(dump.len(), 2);
    assert_eq!((dump[0].address, dump[1].address), (0x8000, 0x8010));
    assert!(rom.hexdump(5, 0x8000, 1).is_err());
}

#[test]
fn list_level_rejects_other_json() {
    for json in ["\"full\"", "-1", "256"] {
        assert!(serde_json::from_str::<ListLevel>(json).is_err(), "{json}");
    }
}

#[test]
fn output_errors() {
    let t = Temp::new();
    t.write("test.asm", "  .db 1\n");
    let request = t.request(AssembleOptions::default());
    let cancel = AtomicBool::new(false);
    // The default output path is reported when it cannot be written.
    fs::create_dir(t.0.join("test.nes")).unwrap();
    let (r, _) = nesasm_core::build(&request, None, &cancel);
    let d = r.diagnostics.last().unwrap();
    assert_eq!(d.code, DiagnosticCode::Output);
    assert!(d.message.starts_with("Output path is a directory"), "{d:?}");
    assert_eq!(d.location.file, Path::new("test.nes"));
    let r = nesasm_core::assemble(&request);
    let write = |output: &str| {
        nesasm_core::write_artifacts(
            &r,
            Path::new("test.asm"),
            Some(Path::new(output)),
            &t.0,
            None,
            &AssembleOptions::default(),
        )
    };
    assert_eq!(
        write("test.asm").unwrap_err(),
        "Output would overwrite an input dependency"
    );
    // The output directory cannot be created where a file is.
    t.write("file.txt", "x");
    assert!(write("file.txt/rom.nes").is_err());
    assert!(t.0.join("file.txt").is_file());
}

#[test]
fn sjis_listing_is_encoded() {
    let t = Temp::new();
    let (source, _, _) = encoding_rs::SHIFT_JIS.encode("  .list\n  .db 1 ; あ\n");
    t.write("test.asm", &source);
    let options = AssembleOptions {
        encoding: SourceEncoding::Sjis,
        ..AssembleOptions::default()
    };
    let (r, _) = nesasm_core::build(&t.request(options), None, &AtomicBool::new(false));
    assert!(r.success, "{:?}", r.diagnostics);
    let listing = fs::read(t.0.join("test.lst")).unwrap();
    assert!(listing.windows(2).any(|w| w == [0x82, 0xa0]));
}

#[test]
fn small_payloads_and_unrelocated_symbols() {
    // Too small to hold $FFFA, or cut off inside the vectors: no vectors.
    let rom = Rom::from_file(&[0xea; 0x100]).unwrap();
    assert!(rom.vectors().is_empty());
    assert!(
        Rom::from_file(&[0xea; 0x1ffc])
            .unwrap()
            .vectors()
            .is_empty()
    );
    assert!(
        Rom::from_file(&[0xea; 0x1ffe])
            .unwrap()
            .vectors()
            .is_empty()
    );
    let symbol = nesasm_core::Symbol {
        name: "proc".into(),
        value: 0x8000,
        bank: BankRef::Procedure,
        page: None,
        location: nesasm_core::SourceLocation {
            file: "a.asm".into(),
            line: 1,
            column: None,
        },
        public: true,
        size: 0,
        data_type: None,
    };
    let symbols = [("proc".to_owned(), symbol)].into_iter().collect();
    let code = rom.disassemble(0, 0x8000, 1, &symbols).unwrap();
    assert!(code[0].labels.is_empty());
}

#[test]
fn write_disassembly_replaces_only_unedited_generated_files() {
    let t = Temp::new();
    let r =
        t.run("  .inesprg 1\n  .ineschr 1\n  .bank 1\n  .org $FFFA\n  .dw 0, 0, 0\n  .bank 2\n");
    assert!(r.success, "{:?}", r.diagnostics);
    let mut rom = r.header.clone();
    rom.extend(&r.binary);
    let never = AtomicBool::new(false);
    let d = nesasm_core::disasm::disassemble(
        &rom,
        None,
        &nesasm_core::disasm::DisasmConfig::default(),
        Path::new("gen/game.asm"),
        &never,
    )
    .unwrap();
    let write = || nesasm_core::write_disassembly(&d, &t.0, None, Path::new("game.nes"));
    let artifacts = write().unwrap();
    let kinds: Vec<ArtifactKind> = artifacts.iter().map(|a| a.kind).collect();
    assert_eq!(kinds, [ArtifactKind::Source, ArtifactKind::Chr]);
    assert_eq!(fs::read(t.0.join("gen/game.chr")).unwrap(), vec![0; 8192]);
    assert_eq!(serde_json::to_value(ArtifactKind::Chr).unwrap(), "chr");
    // Unedited output is replaced; an edited CHR file is kept.
    write().unwrap();
    fs::write(t.0.join("gen/game.chr"), vec![1; 8192]).unwrap();
    let e = write().unwrap_err();
    assert!(
        e.contains("game.chr exists and is not an unedited generated file"),
        "{e}"
    );
    fs::write(t.0.join("gen/game.chr"), vec![0; 8192]).unwrap();
    write().unwrap();
    // The CHR file is replaced only with its generated source.
    fs::remove_file(t.0.join("gen/game.asm")).unwrap();
    let e = write().unwrap_err();
    assert!(
        e.contains("game.chr exists and is not an unedited generated file"),
        "{e}"
    );
    fs::remove_file(t.0.join("gen/game.chr")).unwrap();
    write().unwrap();
    let source = fs::read_to_string(t.0.join("gen/game.asm")).unwrap();
    fs::write(
        t.0.join("gen/game.asm"),
        source.replace(".inesprg 1", ".inesprg 1 ; edited"),
    )
    .unwrap();
    let e = write().unwrap_err();
    assert!(
        e.contains("game.asm exists and is not an unedited generated file"),
        "{e}"
    );
    // The ROM is never overwritten, and only .asm sources are written.
    let d = nesasm_core::disasm::disassemble(
        &rom,
        None,
        &nesasm_core::disasm::DisasmConfig::default(),
        Path::new("game.ASM"),
        &never,
    )
    .unwrap();
    let e = nesasm_core::write_disassembly(&d, &t.0, None, Path::new("game.chr")).unwrap_err();
    assert_eq!(e, "Output would overwrite the ROM");
    let d = nesasm_core::disasm::disassemble(
        &rom,
        None,
        &nesasm_core::disasm::DisasmConfig::default(),
        Path::new("game.txt"),
        &never,
    )
    .unwrap();
    let e = nesasm_core::write_disassembly(&d, &t.0, None, Path::new("game.nes")).unwrap_err();
    assert_eq!(e, "Source output must have a .asm extension");
}

#[cfg(unix)]
#[test]
fn write_disassembly_does_not_follow_links_to_other_files() {
    let t = Temp::new();
    let r = t.run(
        "  .inesprg 1
  .ineschr 1
  .bank 1
  .org $FFFA
  .dw 0, 0, 0
  .bank 2
",
    );
    assert!(r.success, "{:?}", r.diagnostics);
    let mut rom = r.header.clone();
    rom.extend(&r.binary);
    let never = AtomicBool::new(false);
    let d = nesasm_core::disasm::disassemble(
        &rom,
        None,
        &nesasm_core::disasm::DisasmConfig::default(),
        Path::new("game.asm"),
        &never,
    )
    .unwrap();
    t.write("backup.nes", b"keep");
    std::os::unix::fs::symlink(t.0.join("backup.nes"), t.0.join("game.chr")).unwrap();
    let e = nesasm_core::write_disassembly(&d, &t.0, None, Path::new("game.nes")).unwrap_err();
    assert!(e.contains("backup.nes is not a .chr file"), "{e}");
    assert_eq!(fs::read(t.0.join("backup.nes")).unwrap(), b"keep");
}

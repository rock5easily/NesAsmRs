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
fn build_overwrites_long_paths_within_the_root() {
    // Existing files whose canonical paths are long, in a directory whose
    // path is not: on Windows, only the files' paths keep the `\\?\` prefix.
    let t = Temp::new();
    let dir = common::long_dir(&t.0);
    t.write(&format!("{dir}source_file.asm"), "  .list\n  .db 1\n");
    let mut request = t.request(AssembleOptions::default());
    request.input = format!("{dir}source_file.asm").into();
    let output = format!("{dir}output_file.nes");
    let cancel = AtomicBool::new(false);
    for _ in 0..2 {
        let (r, artifacts) = nesasm_core::build(&request, Some(Path::new(&output)), &cancel);
        assert!(r.success, "{:?}", r.diagnostics);
        let kinds: Vec<_> = artifacts.iter().map(|a| a.kind).collect();
        assert_eq!(kinds, [ArtifactKind::Rom, ArtifactKind::Listing]);
    }
    assert!(t.0.join(&dir).join("output_file.lst").is_file());
    // Hidden directories stay rejected.
    let hidden = format!("{dir}.hidden_directory/output_file.nes");
    t.write(&hidden, "");
    let (r, artifacts) = nesasm_core::build(&request, Some(Path::new(&hidden)), &cancel);
    assert!(artifacts.is_empty());
    let message = &r.diagnostics.last().unwrap().message;
    assert!(
        message.contains("must not contain hidden files"),
        "{message}"
    );
}

/// Opens `path` sharing it only for reading, as some editors and virus
/// scanners do, so that it cannot be renamed or replaced.
#[cfg(windows)]
fn lock(path: &Path) -> fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .share_mode(1) // FILE_SHARE_READ
        .open(path)
        .unwrap()
}

/// The files in `dir`, sorted, with their contents.
#[cfg(windows)]
fn contents(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            (name, fs::read(entry.path()).unwrap_or_default())
        })
        .collect();
    files.sort();
    files
}

#[cfg(windows)]
#[test]
fn build_keeps_all_artifacts_when_one_cannot_be_replaced() {
    let t = Temp::new();
    t.write("test.asm", "  .list\n  .db 1\n");
    let request = t.request(AssembleOptions::default());
    let cancel = AtomicBool::new(false);
    let (r, _) = nesasm_core::build(&request, None, &cancel);
    assert!(r.success, "{:?}", r.diagnostics);
    let before = contents(&t.0);
    // The ROM is replaced first, then the listing fails: the ROM is restored.
    let listing = lock(&t.0.join("test.lst"));
    t.write("test.asm", "  .list\n  .db 2\n");
    let (r, artifacts) = nesasm_core::build(&request, None, &cancel);
    assert!(artifacts.is_empty());
    let message = &r.diagnostics.last().unwrap().message;
    assert!(
        message.starts_with("Could not write ")
            && message.contains("test.lst: ")
            && message.ends_with("; nothing was changed"),
        "{message}"
    );
    let mut expected = before;
    expected[0].1 = fs::read(t.0.join("test.asm")).unwrap();
    assert_eq!(contents(&t.0), expected);
    drop(listing);
    let (r, artifacts) = nesasm_core::build(&request, None, &cancel);
    assert!(r.success && artifacts.len() == 2, "{:?}", r.diagnostics);
    assert_eq!(fs::read(t.0.join("test.nes")).unwrap()[16], 2);
}

#[cfg(windows)]
#[test]
fn export_disassembly_keeps_all_files_when_one_cannot_be_replaced() {
    let t = Temp::new();
    let r = t.run(
        "  .inesprg 1\n  .ineschr 1\n  .bank 1\n  .org $FFFA\n  .dw 0, 0, 0\n  .bank 2\n  .db 1\n",
    );
    assert!(r.success, "{:?}", r.diagnostics);
    let mut rom = r.header.clone();
    rom.extend(&r.binary);
    let never = AtomicBool::new(false);
    let disassemble = |rom: &[u8]| {
        let config = nesasm_core::disasm::DisasmConfig::default();
        nesasm_core::disasm::disassemble(rom, None, &config, Path::new("out/game.asm"), &never)
            .unwrap()
    };
    let export = |d: &nesasm_core::disasm::Disassembly| {
        nesasm_core::export_disassembly(d, &t.0, Some(&t.0), Path::new("game.nes"), &never)
    };
    export(&disassemble(&rom)).unwrap();
    let out = t.0.join("out");
    let before = contents(&out);
    assert_eq!(before.len(), 2, "{before:?}");
    // Other CHR data changes both files; the source is written first.
    *rom.last_mut().unwrap() = 0x55;
    let d = disassemble(&rom);
    let chr = lock(&out.join("game.chr"));
    let e = export(&d).unwrap_err();
    assert!(
        e.contains("game.chr: ") && e.ends_with("; nothing was changed"),
        "{e}"
    );
    assert_eq!(contents(&out), before);
    drop(chr);
    let e = export(&d).unwrap();
    assert_eq!(e.artifacts.len(), 2);
    assert_eq!(
        *fs::read(out.join("game.chr")).unwrap().last().unwrap(),
        0x55
    );
}

#[test]
fn relative_to_root_compares_components() {
    let relative =
        |path: &str, root: &str| nesasm_core::relative_to_root(Path::new(path), Path::new(root));
    let (root, inside, sibling) = if cfg!(windows) {
        (r"C:\project", r"C:\project\src\a.asm", r"C:\project2\a.asm")
    } else {
        ("/project", "/project/src/a.asm", "/project2/a.asm")
    };
    assert_eq!(relative(inside, root), Some(Path::new("src").join("a.asm")));
    assert_eq!(relative(root, root), Some(Path::new("").to_owned()));
    assert_eq!(relative(sibling, root), None);
    assert_eq!(relative(root, inside), None);
    if cfg!(windows) {
        // The verbatim prefix of either path is ignored.
        let a = Path::new("a.asm").to_owned();
        assert_eq!(relative(r"\\?\C:\project\a.asm", root), Some(a.clone()));
        assert_eq!(
            relative(r"C:\project\a.asm", r"\\?\C:\project"),
            Some(a.clone())
        );
        assert_eq!(
            relative(r"\\?\UNC\server\share\a.asm", r"\\server\share"),
            Some(a)
        );
        assert_eq!(relative(r"\\?\D:\project\a.asm", root), None);
        assert_eq!(relative(r"\\?\C:\project2\a.asm", root), None);
    }
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
    let write = || nesasm_core::write_disassembly(&d, &t.0, None, Path::new("game.nes"), &never);
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
    let e =
        nesasm_core::write_disassembly(&d, &t.0, None, Path::new("game.chr"), &never).unwrap_err();
    assert_eq!(e, "Output would overwrite the ROM");
    let d = nesasm_core::disasm::disassemble(
        &rom,
        None,
        &nesasm_core::disasm::DisasmConfig::default(),
        Path::new("game.txt"),
        &never,
    )
    .unwrap();
    let e =
        nesasm_core::write_disassembly(&d, &t.0, None, Path::new("game.nes"), &never).unwrap_err();
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
    let e =
        nesasm_core::write_disassembly(&d, &t.0, None, Path::new("game.nes"), &never).unwrap_err();
    assert!(e.contains("backup.nes is not a .chr file"), "{e}");
    assert_eq!(fs::read(t.0.join("backup.nes")).unwrap(), b"keep");
}

#[test]
fn write_disassembly_protects_split_sources_and_chr() {
    let t = Temp::new();
    let r =
        t.run("  .inesprg 1\n  .ineschr 1\n  .bank 1\n  .org $FFFA\n  .dw 0, 0, 0\n  .bank 2\n");
    assert!(r.success, "{:?}", r.diagnostics);
    let mut rom = r.header.clone();
    rom.extend(&r.binary);
    let never = AtomicBool::new(false);
    let config =
        nesasm_core::disasm::parse_config(r#"{"options": {"split_banks": true}}"#).unwrap();
    let d = nesasm_core::disasm::disassemble(&rom, None, &config, Path::new("game.asm"), &never)
        .unwrap();
    let write = || nesasm_core::write_disassembly(&d, &t.0, None, Path::new("game.nes"), &never);
    let kinds: Vec<ArtifactKind> = write().unwrap().iter().map(|a| a.kind).collect();
    assert_eq!(
        kinds,
        // The main file, a file per bank (including the CHR bank) and the CHR file.
        [
            ArtifactKind::Source,
            ArtifactKind::Source,
            ArtifactKind::Source,
            ArtifactKind::Source,
            ArtifactKind::Chr
        ]
    );
    write().unwrap();
    // A changed CHR file or bank file is kept.
    fs::write(t.0.join("game.chr"), vec![1; 8192]).unwrap();
    let e = write().unwrap_err();
    assert!(
        e.contains("game.chr exists and is not an unedited generated file"),
        "{e}"
    );
    fs::write(t.0.join("game.chr"), vec![0; 8192]).unwrap();
    let bank = fs::read_to_string(t.0.join("game_bank01.asm")).unwrap();
    fs::write(
        t.0.join("game_bank01.asm"),
        bank.replace(".bank 1", ".bank 1 ; mine"),
    )
    .unwrap();
    let e = write().unwrap_err();
    assert!(
        e.contains("game_bank01.asm exists and is not an unedited generated file"),
        "{e}"
    );
}

#[test]
fn write_disassembly_keeps_sources_that_are_not_utf8() {
    let t = Temp::new();
    let r = t.run("  .inesprg 1\n  .bank 1\n  .org $FFFA\n  .dw 0, 0, 0\n");
    assert!(r.success, "{:?}", r.diagnostics);
    let mut rom = r.header.clone();
    rom.extend(&r.binary);
    let never = AtomicBool::new(false);
    // A comment with U+FFFD, which a lossy conversion would also produce.
    let config = nesasm_core::disasm::parse_config(
        r#"{"comments": [{"address": "$E000", "text": "�", "position": "above"}]}"#,
    )
    .unwrap();
    let d = nesasm_core::disasm::disassemble(&rom, None, &config, Path::new("game.asm"), &never)
        .unwrap();
    let write = || nesasm_core::write_disassembly(&d, &t.0, None, Path::new("game.nes"), &never);
    write().unwrap();
    let path = t.0.join("game.asm");
    let bytes = fs::read(&path).unwrap();
    let at = bytes
        .windows(3)
        .position(|w| w == [0xef, 0xbf, 0xbd])
        .unwrap();
    let mut edited = bytes[..at].to_vec();
    edited.push(0xff);
    edited.extend(&bytes[at + 3..]);
    fs::write(&path, &edited).unwrap();
    let e = write().unwrap_err();
    assert!(
        e.contains("game.asm exists and is not an unedited generated file"),
        "{e}"
    );
    assert_eq!(fs::read(&path).unwrap(), edited);
}

#[test]
fn write_disassembly_writes_nothing_when_cancelled() {
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
        Path::new("out/game.asm"),
        &never,
    )
    .unwrap();
    let cancelled = AtomicBool::new(true);
    let e = nesasm_core::write_disassembly(&d, &t.0, None, Path::new("game.nes"), &cancelled)
        .unwrap_err();
    assert_eq!(e, "Cancelled before writing; nothing was written");
    // Neither the files nor their staging copies remain.
    assert_eq!(fs::read_dir(t.0.join("out")).unwrap().count(), 0);
}

/// An 18-bank NROM-style ROM (banks 0 to 17) and its disassembly into
/// `<dir>game.asm` and a file per bank.
fn eighteen_banks(t: &Temp, dir: &str) -> (Vec<u8>, nesasm_core::disasm::Disassembly) {
    let r = t.run("  .inesprg 9\n  .bank 17\n  .org $FFFA\n  .dw 0, 0, 0\n");
    assert!(r.success, "{:?}", r.diagnostics);
    let mut rom = r.header.clone();
    rom.extend(&r.binary);
    let config =
        nesasm_core::disasm::parse_config(r#"{"options": {"split_banks": true}}"#).unwrap();
    let never = AtomicBool::new(false);
    let source = format!("{dir}game.asm");
    let d =
        nesasm_core::disasm::disassemble(&rom, None, &config, Path::new(&source), &never).unwrap();
    (rom, d)
}

/// Writes `d` as versions before hexadecimal bank names did: two spaces of
/// indentation and decimal names (`game_bank10.asm` for bank 10), with the
/// checksums of that text. `dir` is the directory of the source, as given to
/// [`eighteen_banks`].
fn write_old_format(t: &Temp, d: &nesasm_core::disasm::Disassembly, dir: &str) {
    let fnv = |text: &str| {
        text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, b| {
            (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
    };
    let decimal = |hex: &str| usize::from_str_radix(hex, 16).unwrap();
    let include = format!("\t.include \"{dir}game_bank");
    for (path, text) in d.files() {
        let (_, body) = text.split_once('\n').unwrap();
        let body: String = body
            .lines()
            .map(|line| match line.strip_prefix(&include) {
                Some(rest) => {
                    let bank = decimal(rest.trim_end_matches(".asm\""));
                    format!("  .include \"{dir}game_bank{bank:02}.asm\"\n")
                }
                None => match line.strip_prefix('\t') {
                    Some(rest) => format!("  {rest}\n"),
                    None => format!("{line}\n"),
                },
            })
            .collect();
        let name = match body.lines().next().unwrap().strip_prefix("  .bank ") {
            Some(bank) => format!("{dir}game_bank{:02}.asm", bank.parse::<usize>().unwrap()),
            None => path.to_string_lossy().into_owned(),
        };
        let marker = format!(
            "; Generated by nesasm disassembler; checksum {:016x}\n",
            fnv(&body)
        );
        t.write(&name, format!("{marker}{body}"));
    }
}

fn first_body_line(t: &Temp, name: &str) -> String {
    let text = fs::read_to_string(t.0.join(name)).unwrap();
    text.lines().nth(1).unwrap().to_owned()
}

#[test]
fn export_disassembly_migrates_decimal_bank_file_names() {
    let t = Temp::new();
    let (rom, d) = eighteen_banks(&t, "");
    write_old_format(&t, &d, "");
    assert_eq!(first_body_line(&t, "game_bank10.asm"), "  .bank 10");
    let never = AtomicBool::new(false);
    let export =
        || nesasm_core::export_disassembly(&d, &t.0, Some(&t.0), Path::new("game.nes"), &never);
    let e = export().unwrap();
    // The old main file included every file, so they are generated again
    // under their new names; the old names past $11 are removed.
    let names = |paths: Vec<&Path>| -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(
        names(e.removed.iter().map(|p| p.as_path()).collect()),
        (12..18)
            .map(|b| format!("game_bank{b}.asm"))
            .collect::<Vec<_>>()
    );
    assert!(e.warnings.is_empty());
    let written = names(e.artifacts.iter().map(|a| a.path.as_path()).collect());
    let mut expected = vec!["game.asm".to_owned()];
    expected.extend((0..18).map(|b| format!("game_bank{b:02X}.asm")));
    assert_eq!(written, expected);
    for (bank, name) in [
        (9, "game_bank09.asm"),
        (10, "game_bank0A.asm"),
        (16, "game_bank10.asm"),
        (17, "game_bank11.asm"),
    ] {
        assert_eq!(first_body_line(&t, name), format!("\t.bank {bank}"));
    }
    for b in 12..18 {
        assert!(!t.0.join(format!("game_bank{b}.asm")).exists());
    }
    // The includes name the written files, which assemble to the ROM.
    let main = fs::read_to_string(t.0.join("game.asm")).unwrap();
    let included: Vec<String> = main
        .lines()
        .filter_map(|l| l.strip_prefix("\t.include \""))
        .map(|l| l.trim_end_matches('"').to_owned())
        .collect();
    assert_eq!(included, expected[1..]);
    let request = nesasm_core::AssembleRequest {
        input: "game.asm".into(),
        working_directory: t.0.clone(),
        ..Default::default()
    };
    let r = nesasm_core::assemble(&request);
    assert!(r.success, "{:?}", r.diagnostics);
    let mut rebuilt = r.header.clone();
    rebuilt.extend(&r.binary);
    assert_eq!(rebuilt, rom);
    // Unedited output is written again; nothing is left to remove.
    let e = export().unwrap();
    assert!(e.removed.is_empty());
    assert_eq!(e.artifacts.len(), 19);
}

#[test]
fn export_disassembly_does_not_mix_up_banks_of_old_names() {
    let t = Temp::new();
    let (rom, d) = eighteen_banks(&t, "");
    write_old_format(&t, &d, "");
    let never = AtomicBool::new(false);
    let export =
        || nesasm_core::export_disassembly(&d, &t.0, Some(&t.0), Path::new("game.nes"), &never);
    let old_main = fs::read(t.0.join("game.asm")).unwrap();
    let old_bank10 = fs::read(t.0.join("game_bank10.asm")).unwrap();
    // Without the main file that included it, bank 10's old file is not
    // replaced by bank 16, which now has its name; nothing is written.
    fs::remove_file(t.0.join("game.asm")).unwrap();
    let e = export().unwrap_err();
    assert!(
        e.contains("game_bank10.asm exists and is the generated file of bank 10, not of bank 16"),
        "{e}"
    );
    assert_eq!(fs::read(t.0.join("game_bank10.asm")).unwrap(), old_bank10);
    assert!(!t.0.join("game.asm").exists());
    assert!(!t.0.join("game_bank0A.asm").exists());
    // An edited old file is kept, even under a name the new source uses.
    fs::write(t.0.join("game.asm"), &old_main).unwrap();
    let old_bank11 = fs::read_to_string(t.0.join("game_bank11.asm")).unwrap();
    let edited = old_bank11.replace(".bank 11", ".bank 11 ; mine");
    fs::write(t.0.join("game_bank11.asm"), &edited).unwrap();
    let e = export().unwrap_err();
    assert!(
        e.contains("game_bank11.asm exists and is not an unedited generated file"),
        "{e}"
    );
    assert_eq!(
        fs::read_to_string(t.0.join("game_bank11.asm")).unwrap(),
        edited
    );
    assert_eq!(fs::read(t.0.join("game.asm")).unwrap(), old_main);
    // An edited old file the new source no longer includes is not removed.
    fs::write(t.0.join("game_bank11.asm"), &old_bank11).unwrap();
    let old_bank13 = fs::read_to_string(t.0.join("game_bank13.asm")).unwrap();
    let edited = old_bank13.replace(".bank 13", ".bank 13 ; mine");
    fs::write(t.0.join("game_bank13.asm"), &edited).unwrap();
    let e = export().unwrap();
    assert_eq!(e.removed.len(), 5);
    assert!(!e.removed.iter().any(|p| p.ends_with("game_bank13.asm")));
    assert_eq!(
        fs::read_to_string(t.0.join("game_bank13.asm")).unwrap(),
        edited
    );
    assert_eq!(first_body_line(&t, "game_bank11.asm"), "\t.bank 17");
    // An unedited bank file is not replaced by a main source.
    let bank0 = fs::read(t.0.join("game_bank00.asm")).unwrap();
    fs::write(t.0.join("other.asm"), &bank0).unwrap();
    let whole = nesasm_core::disasm::disassemble(
        &rom,
        None,
        &nesasm_core::disasm::DisasmConfig::default(),
        Path::new("other.asm"),
        &never,
    )
    .unwrap();
    let e =
        nesasm_core::export_disassembly(&whole, &t.0, Some(&t.0), Path::new("game.nes"), &never)
            .unwrap_err();
    assert!(
        e.contains("other.asm exists and is not an unedited generated file"),
        "{e}"
    );
    assert_eq!(fs::read(t.0.join("other.asm")).unwrap(), bank0);
}

#[cfg(unix)]
#[test]
fn export_disassembly_does_not_remove_through_links() {
    let t = Temp::new();
    let (_, d) = eighteen_banks(&t, "");
    write_old_format(&t, &d, "");
    // The old main file includes game_bank13.asm, a link to an unedited bank
    // file of another source whose name has the same prefix.
    let other = t.0.join("game_bank_backup_bank0D.asm");
    fs::rename(t.0.join("game_bank13.asm"), &other).unwrap();
    let backup = fs::read(&other).unwrap();
    std::os::unix::fs::symlink(&other, t.0.join("game_bank13.asm")).unwrap();
    let never = AtomicBool::new(false);
    let e = nesasm_core::export_disassembly(&d, &t.0, Some(&t.0), Path::new("game.nes"), &never)
        .unwrap();
    assert_eq!(e.removed.len(), 5, "{:?}", e.removed);
    assert!(
        !e.removed
            .iter()
            .any(|p| p.ends_with("game_bank13.asm") || p.ends_with("game_bank_backup_bank0D.asm")),
        "{:?}",
        e.removed
    );
    assert_eq!(fs::read(&other).unwrap(), backup);
    assert!(
        fs::symlink_metadata(t.0.join("game_bank13.asm"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn export_disassembly_migrates_files_with_long_paths() {
    // A directory below 260 characters whose files are longer: on Windows,
    // their canonical paths keep the `\\?\` prefix and the directory's does not.
    let t = Temp::new();
    let dir = common::long_dir(&t.0);
    let (_, d) = eighteen_banks(&t, &dir);
    write_old_format(&t, &d, &dir);
    let never = AtomicBool::new(false);
    // Within a root, whose own path is short.
    let e = nesasm_core::export_disassembly(&d, &t.0, Some(&t.0), Path::new("game.nes"), &never)
        .unwrap();
    assert_eq!(e.removed.len(), 6, "{:?}", e.removed);
    let text = fs::read_to_string(t.0.join(&dir).join("game_bank10.asm")).unwrap();
    assert_eq!(text.lines().nth(1), Some("\t.bank 16"));
}

/// A mapper 4 ROM hack and its base, of four PRG banks and a CHR bank; the
/// hack changes the last two PRG banks and the CHR bank.
fn mmc3_hack(t: &Temp) -> (Vec<u8>, Vec<u8>) {
    let r = t.run(concat!(
        "  .inesprg 2\n  .ineschr 1\n  .inesmap 4\n",
        "  .bank 3\n  .org $FFFA\n  .dw 0, 0, 0\n  .bank 4\n  .db 1\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    let mut base = r.header.clone();
    base.extend(&r.binary);
    let mut hack = base.clone();
    hack[16 + 2 * 8192 + 0x100] = 1;
    hack[16 + 3 * 8192 + 0x100] = 2;
    hack[16 + 4 * 8192 + 5] = 3;
    (base, hack)
}

/// A generated source of `body`, with its checksum.
fn generated(body: &str) -> String {
    let sum = body.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("; Generated by nesasm disassembler; checksum {sum:016x}\n{body}")
}

#[test]
fn export_disassembly_replaces_a_chr_file_of_changed_banks_with_the_whole_chr() {
    let t = Temp::new();
    // Two CHR banks; the hack changes only the second.
    let r = t.run(concat!(
        "  .inesprg 1\n  .ineschr 2\n  .bank 1\n  .org $FFFA\n  .dw 0, 0, 0\n",
        "  .bank 2\n  .db 1\n  .bank 3\n  .db 2\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    let mut base = r.header.clone();
    base.extend(&r.binary);
    let mut hack = base.clone();
    hack[16 + 16384 + 8192 + 5] = 3;
    t.write("base.nes", &base);
    let never = AtomicBool::new(false);
    let config = nesasm_core::disasm::parse_config(r#"{"base": "base.nes"}"#).unwrap();
    let new = nesasm_core::disasm::disassemble(
        &hack,
        Some(&base),
        &config,
        Path::new("game.asm"),
        &never,
    )
    .unwrap();
    let whole = &hack[16 + 16384..];
    assert_eq!(new.chr_file().unwrap().1, whole);
    let rebuilt = || {
        let request = nesasm_core::AssembleRequest {
            input: "game.asm".into(),
            working_directory: t.0.clone(),
            ..Default::default()
        };
        let r = nesasm_core::assemble(&request);
        assert!(r.success, "{:?}", r.diagnostics);
        let mut rom = r.header.clone();
        rom.extend(&r.binary);
        rom
    };
    // The files of earlier versions: the base ROM included whole, and a CHR
    // file of the changed bank alone, included over it.
    let old_chr = &hack[16 + 16384 + 8192..];
    let fnv = |bytes: &[u8]| {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, b| {
            (hash ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
        })
    };
    let body = new.text().split_once('\n').unwrap().1;
    let body = body
        .replace(
            &format!("checksum {:016x}", fnv(whole)),
            &format!("checksum {:016x}", fnv(old_chr)),
        )
        .replace("\"base.nes\", 16, 16384", "\"base.nes\", 16, 32768")
        .replace(
            "\t.bank 2\n\t.org $0000\n\t.incbin \"game.chr\"\n",
            "\t.bank 3\n\t.org $0000\n\t.incbin \"game.chr\", 0, 8192\n",
        );
    assert!(
        body.contains("16, 32768") && body.contains("0, 8192"),
        "{body}"
    );
    let write_old = || {
        t.write("game.asm", generated(&body));
        t.write("game.chr", old_chr);
    };
    write_old();
    assert_eq!(rebuilt(), hack);
    let e = nesasm_core::export_disassembly(&new, &t.0, Some(&t.0), Path::new("game.nes"), &never)
        .unwrap();
    assert_eq!(
        file_names(e.artifacts.iter().map(|a| a.path.as_path())),
        ["game.asm", "game.chr"]
    );
    assert_eq!(fs::read(t.0.join("game.chr")).unwrap(), whole);
    assert_eq!(rebuilt(), hack);
    // A CHR file changed since it was generated is kept, and nothing is written.
    write_old();
    let mut edited = old_chr.to_vec();
    edited[0] ^= 0xff;
    t.write("game.chr", &edited);
    let e = nesasm_core::export_disassembly(&new, &t.0, Some(&t.0), Path::new("game.nes"), &never)
        .unwrap_err();
    assert!(
        e.contains("game.chr exists and is not an unedited generated file"),
        "{e}"
    );
    assert_eq!(fs::read(t.0.join("game.chr")).unwrap(), edited);
    assert_eq!(
        fs::read_to_string(t.0.join("game.asm")).unwrap(),
        generated(&body)
    );
}

fn file_names<'a>(paths: impl Iterator<Item = &'a Path>) -> Vec<String> {
    paths
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn export_disassembly_replaces_old_chr_and_pair_bank_files() {
    let t = Temp::new();
    let (base, hack) = mmc3_hack(&t);
    t.write("base.nes", &base);
    let never = AtomicBool::new(false);
    let disassemble = |json: &str| {
        let config = nesasm_core::disasm::parse_config(json).unwrap();
        nesasm_core::disasm::disassemble(&hack, Some(&base), &config, Path::new("game.asm"), &never)
            .unwrap()
    };
    let export = |d: &nesasm_core::disasm::Disassembly| {
        nesasm_core::export_disassembly(d, &t.0, Some(&t.0), Path::new("game.nes"), &never)
    };
    let rebuilt = || {
        let request = nesasm_core::AssembleRequest {
            input: "game.asm".into(),
            working_directory: t.0.clone(),
            ..Default::default()
        };
        let r = nesasm_core::assemble(&request);
        assert!(r.success, "{:?}", r.diagnostics);
        let mut rom = r.header.clone();
        rom.extend(&r.binary);
        rom
    };
    // The files of earlier versions: a file per PRG bank, and one for the
    // changed CHR bank.
    let write_old = || {
        let old = disassemble(r#"{"base": "base.nes", "options": {"split_banks": true}}"#);
        let body = old.text().split_once('\n').unwrap().1;
        let (main, chr) = body.split_once("\n\n\t.bank 4\n").unwrap();
        t.write(
            "game.asm",
            generated(&format!("{main}\n\t.include \"game_bank04.asm\"\n")),
        );
        t.write("game_bank04.asm", generated(&format!("\t.bank 4\n{chr}")));
        for (path, text) in &old.files()[1..] {
            t.write(&path.to_string_lossy(), text);
        }
        t.write("game.chr", old.chr_file().unwrap().1);
    };
    write_old();
    assert_eq!(rebuilt(), hack);
    let new = disassemble(
        r#"{"base": "base.nes", "options": {"split_banks": true, "mapper4_fixed_prg_16k": true}}"#,
    );
    let e = export(&new).unwrap();
    assert_eq!(
        file_names(e.artifacts.iter().map(|a| a.path.as_path())),
        ["game.asm", "game_bank02_03.asm", "game.chr"]
    );
    assert_eq!(
        file_names(e.removed.iter().map(|p| p.as_path())),
        ["game_bank02.asm", "game_bank03.asm", "game_bank04.asm"]
    );
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    let main = fs::read_to_string(t.0.join("game.asm")).unwrap();
    assert!(
        main.lines().any(|l| l == "\t.incbin \"game.chr\""),
        "{main}"
    );
    assert_eq!(rebuilt(), hack);
    // An edited old bank file is kept; the unedited ones are removed.
    write_old();
    let bank3 = fs::read_to_string(t.0.join("game_bank03.asm")).unwrap();
    let edited = bank3.replace(".bank 3", ".bank 3 ; mine");
    fs::write(t.0.join("game_bank03.asm"), &edited).unwrap();
    let e = export(&new).unwrap();
    assert_eq!(
        file_names(e.removed.iter().map(|p| p.as_path())),
        ["game_bank02.asm", "game_bank04.asm"]
    );
    assert_eq!(
        fs::read_to_string(t.0.join("game_bank03.asm")).unwrap(),
        edited
    );
    assert_eq!(rebuilt(), hack);
    // A file of the pair's name that was not generated is not replaced.
    let main = fs::read(t.0.join("game.asm")).unwrap();
    fs::write(t.0.join("game_bank02_03.asm"), "  nop\n").unwrap();
    let e = export(&new).unwrap_err();
    assert!(
        e.contains("game_bank02_03.asm exists and is not an unedited generated file"),
        "{e}"
    );
    assert_eq!(fs::read(t.0.join("game.asm")).unwrap(), main);
}

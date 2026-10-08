use nesasm_core::{AssembleOptions, AssembleRequest, AssembleResult, SourceEncoding};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
static COUNTER: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "nesasm-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn run(&self, text: &str, options: AssembleOptions) -> AssembleResult {
        fs::write(self.0.join("test.asm"), text).unwrap();
        nesasm_core::assemble(&AssembleRequest {
            input: "test.asm".into(),
            working_directory: self.0.clone(),
            include_paths: vec![],
            allowed_root: Some(self.0.clone()),
            options,
        })
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture(path: &Path) -> AssembleResult {
    nesasm_core::assemble(&AssembleRequest {
        input: path.file_name().unwrap().into(),
        working_directory: path.parent().unwrap().into(),
        include_paths: vec![],
        allowed_root: None,
        options: AssembleOptions::default(),
    })
}

#[test]
fn existing_fixture_success_and_failure() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Tests");
    for folder in ["AdditionalDirectiveTest", "AdditionalFunctionTest"] {
        for file in fs::read_dir(root.join(folder).join("TestData")).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_none_or(|e| e != "asm") {
                continue;
            }
            let name = path.file_stem().unwrap().to_str().unwrap();
            let success = !(name.starts_with("invalid_")
                || name.starts_with("undefined_")
                || name.starts_with("no_public")
                || name == "no_catbank_sample"
                || name == "no_defined_regionsize_sample"
                || name == "insufficient_regionsize_sample");
            let result = fixture(&path);
            assert_eq!(result.success, success, "{name}: {:?}", result.diagnostics);
            if !success {
                assert!(result.binary.is_empty());
                assert!(!result.diagnostics.is_empty());
            }
        }
    }
}
#[test]
fn nes_program_and_header() {
    let t = Temp::new();
    let r=t.run("  .inesprg 1\n  .inesmap 2\n  .inesmir 1\n  .bank 0\n  .org $8000\nReset:\n  lda #$12\n  sta <$34\n  bne Reset\n  .bank 1\n  .org $fffa\n  .dw Reset,Reset,Reset\n",AssembleOptions::default());
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..6], &[0xa9, 0x12, 0x85, 0x34, 0xd0, 0xfa]);
    assert_eq!(r.binary.len(), 16384);
    assert_eq!(&r.header[..8], &[b'N', b'E', b'S', 26, 1, 0, 0x21, 0]);
    assert_eq!(&r.map[..6], &[0x82; 6]);
    assert_eq!(&r.binary[16378..], &[0, 128, 0, 128, 0, 128]);
}
#[test]
fn expressions_and_macro() {
    let t = Temp::new();
    let r=t.run("SUM .func (\\1+\\2)\nemit .macro\n  .db \\1\n  .endm\n  .bank 0\n  .org $8000\n  emit SUM(3,4)\n  .db (2+3*4),LOW($1234),HIGH($1234),-1\n",AssembleOptions::default());
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..5], &[7, 14, 0x34, 0x12, 255]);
}
#[test]
fn catbank_map_wrap() {
    let t = Temp::new();
    let r = t.run(
        "  .catbank 0\n  .bank 0\n  .org $ffff\n  .db 1,2\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(r.binary[8191], 1);
    assert_eq!(r.binary[8192], 2);
    assert_eq!(r.map[8191], 0xe2);
    assert_eq!(r.map[8192], 2);
}
#[test]
fn forward_public_label() {
    let t = Temp::new();
    let r = t.run(
        "  .org $8000\n  jsr Global.local\nGlobal:\n.local .public\n  rts\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..4], &[0x20, 3, 128, 0x60]);
}
#[test]
fn branch_boundaries_and_overflow() {
    let t = Temp::new();
    for (target, success) in [
        ("$7f82", true),
        ("$8081", true),
        ("$7f81", false),
        ("$8082", false),
    ] {
        let r = t.run(
            &format!("  .org $8000\n  bne {target}\n"),
            AssembleOptions::default(),
        );
        assert_eq!(r.success, success, "{target}");
    }
}
#[test]
fn data_and_zero_page_overflow() {
    let t = Temp::new();
    for text in [
        "  .db 256",
        "  lda #256",
        "  .zp\n  .ds 257",
        "  .org $9fff\n  .db 1,2",
        "  .db 1/0",
    ] {
        assert!(!t.run(text, AssembleOptions::default()).success, "{text}");
    }
}
#[test]
fn check_has_no_output_and_dependency_trace() {
    let t = Temp::new();
    fs::write(t.0.join("include.asm"), "  lda #300").unwrap();
    let r = t.run("  .include \"include.asm\"", AssembleOptions::default());
    assert!(!r.success);
    assert_eq!(r.dependencies.len(), 2);
    assert_eq!(r.diagnostics[0].location.line, 1);
    assert!(r.diagnostics[0].location.file.ends_with("include.asm"));
    assert_eq!(r.diagnostics[0].expansion_trace.len(), 1);
    assert!(!t.0.join("test.nes").exists());
}
#[test]
fn incbin_slice_and_tile() {
    let t = Temp::new();
    fs::write(t.0.join("data.bin"), [1, 2, 3, 4]).unwrap();
    let r = t.run(
        "  .incbin \"data.bin\",1,2\n  .defchr $11111111,$22222222,0,0,0,0,0,0",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..4], &[2, 3, 0xff, 0]);
    assert_eq!(&r.binary[10..12], &[0, 0xff]);
}
#[test]
fn defchr_uses_one_row_per_argument() {
    let t = Temp::new();
    let r = t.run(
        "  .defchr $00000000,$01230123,$33333333,$10000001,0,0,0,$32100123",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(
        &r.binary[..16],
        &[
            0x00, 0x55, 0xff, 0x81, 0, 0, 0, 0xa5, // plane 0
            0x00, 0x33, 0xff, 0x00, 0, 0, 0, 0xc3, // plane 1
        ]
    );
    let r = t.run(
        "  .defchr $00000004,0,0,0,0,0,0,0",
        AssembleOptions::default(),
    );
    assert!(!r.success);
}
#[test]
fn macro_argument_count() {
    let t = Temp::new();
    let r = t.run(
        "count .macro\n  .db \\#\n  .endm\n  count\n  count 1\n  count 1,2\n  count 1,2,3\n  count 1,2,3,4,5,6,7,8,9\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..5], &[0, 1, 2, 3, 9]);
}
#[test]
fn root_boundary_and_output_protection() {
    let t = Temp::new();
    let r = t.run("  .include \"../outside.asm\"", AssembleOptions::default());
    assert!(!r.success);
    let r = t.run("  .db 1", AssembleOptions::default());
    assert!(r.success);
    assert!(
        nesasm_core::write_artifacts(
            &r,
            Path::new("test.asm"),
            Some(Path::new("../escape.nes")),
            &t.0,
            Some(&t.0),
            &AssembleOptions::default()
        )
        .is_err()
    );
    assert!(
        nesasm_core::write_artifacts(
            &r,
            Path::new("test.asm"),
            Some(Path::new("test.asm")),
            &t.0,
            Some(&t.0),
            &AssembleOptions::default()
        )
        .is_err()
    );
}
#[test]
fn explicit_sjis_and_invalid_utf8() {
    let t = Temp::new();
    let (encoded, _, _) = encoding_rs::SHIFT_JIS.encode("; 日本語\n  .db 1");
    fs::write(t.0.join("test.asm"), &encoded).unwrap();
    let mut request = AssembleRequest {
        input: "test.asm".into(),
        working_directory: t.0.clone(),
        include_paths: vec![],
        allowed_root: None,
        options: AssembleOptions::default(),
    };
    assert!(!nesasm_core::assemble(&request).success);
    request.options.encoding = SourceEncoding::Sjis;
    assert!(nesasm_core::assemble(&request).success);
}
#[test]
fn regions_can_be_negative_and_incomplete() {
    let t = Temp::new();
    let r=t.run("  .bank 1\n  .org $8000\n  .beginregion \"back\"\n  .bank 0\n  .org $ffff\n  .endregion \"back\"\n  .beginregion \"incomplete\"\n",AssembleOptions::default());
    assert!(r.success);
    assert_eq!(r.regions["back"].size, Some(-1));
    assert_eq!(r.regions["incomplete"].size, None);
}
#[test]
fn reference_topics() {
    for topic in [
        "index",
        "instructions",
        "directives",
        "expressions",
        "options",
    ] {
        assert!(!nesasm_core::reference(Some(topic)).unwrap().is_empty());
    }
    assert!(nesasm_core::reference(Some("unknown")).is_err());
}

#[test]
fn json_contract_uses_legacy_type_names() {
    let t = Temp::new();
    let result = t.run("Data: .db 1,2", AssembleOptions::default());
    assert!(result.success);
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["symbols"]["Data"]["data_type"], "DB");
    let artifacts = nesasm_core::write_artifacts(
        &result,
        Path::new("test.asm"),
        None,
        &t.0,
        Some(&t.0),
        &AssembleOptions::default(),
    )
    .unwrap();
    assert_eq!(serde_json::to_value(&artifacts).unwrap()[0]["kind"], "rom");
    let failed = t.run("  .db 256", AssembleOptions::default());
    assert_eq!(
        serde_json::to_value(&failed).unwrap()["diagnostics"][0]["severity"],
        "error"
    );
}

#[test]
fn invalid_secondary_output_preserves_existing_rom() {
    let t = Temp::new();
    let result = t.run("  .list\n  .db 1", AssembleOptions::default());
    assert!(result.success);
    fs::write(t.0.join("test.nes"), b"existing ROM").unwrap();
    fs::create_dir(t.0.join("test.lst")).unwrap();
    assert!(
        nesasm_core::write_artifacts(
            &result,
            Path::new("test.asm"),
            None,
            &t.0,
            Some(&t.0),
            &AssembleOptions::default()
        )
        .is_err()
    );
    assert_eq!(fs::read(t.0.join("test.nes")).unwrap(), b"existing ROM");
    assert!(fs::read_dir(&t.0).unwrap().all(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_none_or(|ext| ext != "tmp")
    }));
}

#[test]
fn incbin_reads_only_the_requested_range() {
    use std::io::{Seek, SeekFrom, Write};
    let t = Temp::new();
    let mut binary = fs::File::create(t.0.join("large.bin")).unwrap();
    let length = 64 * 1024 * 1024;
    binary.set_len(length).unwrap();
    binary.seek(SeekFrom::Start(length - 1)).unwrap();
    binary.write_all(&[0x42]).unwrap();
    drop(binary);
    let result = t.run(
        &format!("  .incbin \"large.bin\",{},1", length - 1),
        AssembleOptions::default(),
    );
    assert!(result.success, "{:?}", result.diagnostics);
    assert_eq!(result.binary[0], 0x42);
    assert!(
        !t.run("  .incbin \"large.bin\"", AssembleOptions::default())
            .success
    );
}

#[test]
fn macro_definition_must_end_in_its_input_file() {
    let t = Temp::new();
    fs::write(t.0.join("macro-start.asm"), "emit .macro\n  .db \\1\n").unwrap();
    let result = t.run(
        "  .include \"macro-start.asm\"\n  .endm\n  .org $8000\n  emit 42\n",
        AssembleOptions::default(),
    );
    assert!(!result.success);
    assert!(result.binary.is_empty());
}

#[test]
fn pcx_raw_rle_and_planar_tiles() {
    let t = Temp::new();
    for (bpp, planes, rle) in [(8, 1, false), (8, 1, true), (1, 2, false), (1, 2, true)] {
        let mut data = vec![0u8; 128];
        data[0] = 10;
        data[1] = 5;
        data[2] = u8::from(rle);
        data[3] = bpp;
        data[8] = 15;
        data[10] = 15;
        data[65] = planes;
        let stride = if bpp == 8 { 16 } else { 2 };
        data[66] = stride;
        for _ in 0..16 {
            if bpp == 8 {
                data.extend((0..16).map(|x| x % 4));
            } else {
                data.extend([0x55, 0x55, 0x33, 0x33]);
            }
        }
        if rle {
            let raw = data.split_off(128);
            let mut i = 0;
            while i < raw.len() {
                let value = raw[i];
                let mut end = i + 1;
                while end < raw.len() && raw[end] == value && end - i < 63 {
                    end += 1;
                }
                if end - i > 1 || value >= 0xc0 {
                    data.extend([0xc0 | ((end - i) as u8), value]);
                } else {
                    data.push(value);
                }
                i = end;
            }
        }
        fs::write(t.0.join("image.pcx"), &data).unwrap();
        let r = t.run("Tiles: .incchr \"image.pcx\"", AssembleOptions::default());
        assert!(r.success, "{bpp}/{planes}/{rle}: {:?}", r.diagnostics);
        for tile in r.binary[..64].chunks(16) {
            assert_eq!(&tile[..8], &[0x55; 8]);
            assert_eq!(&tile[8..], &[0x33; 8]);
        }
        assert_eq!(r.symbols["Tiles"].size, 64);
        let cropped = t.run(
            "  .incchr \"image.pcx\",8,8,1,1",
            AssembleOptions::default(),
        );
        assert!(cropped.success);
        assert_eq!(&cropped.binary[..16], &r.binary[..16]);
        let invalid = t.run(
            "  .incchr \"image.pcx\",9,8,1,1",
            AssembleOptions::default(),
        );
        assert!(!invalid.success);
    }
    fs::write(t.0.join("image.pcx"), [10, 0]).unwrap();
    assert!(
        !t.run("  .incchr \"image.pcx\"", AssembleOptions::default())
            .success
    );
}

#[cfg(unix)]
#[test]
fn symlink_escape_is_rejected() {
    use std::os::unix::fs::symlink;
    let t = Temp::new();
    let outside = Temp::new();
    fs::write(outside.0.join("external.asm"), "  nop").unwrap();
    symlink(&outside.0, t.0.join("link")).unwrap();
    assert!(
        !t.run(
            "  .include \"link/external.asm\"",
            AssembleOptions::default()
        )
        .success
    );
    let r = t.run("  nop", AssembleOptions::default());
    assert!(
        nesasm_core::write_artifacts(
            &r,
            Path::new("test.asm"),
            Some(Path::new("link/external.nes")),
            &t.0,
            Some(&t.0),
            &AssembleOptions::default()
        )
        .is_err()
    );
}

#[test]
fn nested_input_fails_without_panicking() {
    let t = Temp::new();
    let text = format!("  .db {}1{}", "(".repeat(300), ")".repeat(300));
    assert!(!t.run(&text, AssembleOptions::default()).success);
    assert!(
        !t.run(
            "loop .macro\n  loop\n  .endm\n  loop",
            AssembleOptions::default()
        )
        .success
    );
    assert!(
        !t.run("  .include \"test.asm\"", AssembleOptions::default())
            .success
    );
}

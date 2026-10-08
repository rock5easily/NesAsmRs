use nesasm_core::{AssembleOptions, AssembleRequest, AssembleResult, SourceEncoding};
use std::{
    fs,
    path::{Path, PathBuf},
};
/// A project directory that is removed when the test ends, even on panic.
struct Temp(PathBuf, #[allow(dead_code)] tempfile::TempDir);
impl Temp {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self(dir.path().to_owned(), dir)
    }
    fn run(&self, text: &str, options: AssembleOptions) -> AssembleResult {
        fs::write(self.0.join("test.asm"), text).unwrap();
        nesasm_core::assemble(&AssembleRequest {
            input: "test.asm".into(),
            working_directory: self.0.clone(),
            include_paths: vec![],
            allowed_root: Some(self.0.clone()),
            options,
            ..Default::default()
        })
    }
}
fn fixture(path: &Path) -> AssembleResult {
    nesasm_core::assemble(&AssembleRequest {
        input: path.file_name().unwrap().into(),
        working_directory: path.parent().unwrap().into(),
        include_paths: vec![],
        allowed_root: None,
        options: AssembleOptions::default(),
        ..Default::default()
    })
}

#[test]
fn existing_fixture_success_and_failure() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/compat/fixtures");
    for folder in ["AdditionalDirectiveTest", "AdditionalFunctionTest"] {
        for file in fs::read_dir(root.join(folder)).unwrap() {
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
    fs::write(t.0.join("README.md"), "keep").unwrap();
    for output in [
        "README.md",
        "other.asm",
        ".git/hooks/pre-commit",
        ".git/rom.nes",
        "out/.hidden/rom.nes",
    ] {
        assert!(
            nesasm_core::write_artifacts(
                &r,
                Path::new("test.asm"),
                Some(Path::new(output)),
                &t.0,
                Some(&t.0),
                &AssembleOptions::default()
            )
            .is_err(),
            "{output}"
        );
    }
    assert_eq!(fs::read_to_string(t.0.join("README.md")).unwrap(), "keep");
    assert!(!t.0.join(".git").exists());
    for output in ["out/rom.nes", "out/rom.BIN"] {
        nesasm_core::write_artifacts(
            &r,
            Path::new("test.asm"),
            Some(Path::new(output)),
            &t.0,
            Some(&t.0),
            &AssembleOptions::default(),
        )
        .unwrap();
        assert!(t.0.join(output).is_file());
    }
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
        ..Default::default()
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

#[test]
fn if_condition_changed_between_passes_is_an_error() {
    let t = Temp::new();
    for text in [
        "  .if later\nfoo = 5\n  .endif\nlater = 1\n",
        "  .if later\nbar .rs 1\n  .endif\nlater = 1\n",
        "  .if later\nlabel:\n  nop\n  .endif\nlater = 1\n",
    ] {
        let r = t.run(text, AssembleOptions::default());
        assert!(!r.success, "{text}");
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.message.contains("IF condition changed")),
            "{text}: {:?}",
            r.diagnostics
        );
    }
    // A forward reference that evaluates the same in both passes stays valid.
    let r = t.run(
        "  .if later\n  .db 1\n  .endif\n  .db 2\nlater = 0\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(r.binary[0], 2);
}

#[test]
fn listing_values_without_listing_level() {
    let t = Temp::new();
    let options = AssembleOptions {
        list_level: nesasm_core::ListLevel::Off,
        ..AssembleOptions::default()
    };
    for text in ["  .list\nX = 1\n", "  .list\n  .if 1\n  .db 1\n  .endif\n"] {
        let r = t.run(text, options.clone());
        assert!(r.success, "{text}: {:?}", r.diagnostics);
    }
    let r = t.run(
        "  .list\nX = $1234\n  .if X\n  .endif\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    let listing = r.listing.unwrap();
    assert_eq!(listing.matches("1234").count(), 3, "{listing}");
}

#[test]
fn procedure_ending_on_bank_boundary() {
    let t = Temp::new();
    fs::write(t.0.join("b8191.bin"), vec![1; 8191]).unwrap();
    fs::write(t.0.join("b8192.bin"), vec![2; 8192]).unwrap();
    // A group that fills its bank exactly is accepted; one more byte is rejected.
    let r = t.run(
        "  .procgroup\n  nop\nfoo .proc\n  .incbin \"b8191.bin\"\n  .endp\n  .endprocgroup\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    let r = t.run(
        "  .procgroup\n  nop\nfoo .proc\n  .incbin \"b8192.bin\"\n  .endp\n  .endprocgroup\n",
        AssembleOptions::default(),
    );
    assert!(!r.success);
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.message.contains("too large") || d.message.contains("exceeds")),
        "{:?}",
        r.diagnostics
    );
    let r = t.run(
        "foo .proc\n  .incbin \"b8192.bin\"\n  .endp\nbar .proc\n  rts\n  .endp\n  call foo\n  call bar\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    let foo = &r.symbols["foo"];
    let bar = &r.symbols["bar"];
    assert_ne!((foo.bank, foo.value), (bar.bank, bar.value));
}

#[test]
fn resource_limits_and_cancellation() {
    let t = Temp::new();
    // Exponential expression function expansion stops at the call budget.
    let mut text = String::from("f0 .func \\1\n");
    for i in 1..=12 {
        let call = format!("f{}(\\1)", i - 1);
        text.push_str(&format!("f{i} .func {}\n", [call.as_str(); 4].join("+")));
    }
    text.push_str("  .db f12(1)&255\n");
    let r = t.run(&text, AssembleOptions::default());
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.message.contains("function evaluation limit")),
        "{:?}",
        r.diagnostics
    );
    // Oversized PCX files are rejected before they are read completely.
    let mut pcx = vec![0u8; 8 * 1024 * 1024];
    pcx[0] = 10;
    fs::write(t.0.join("big.pcx"), pcx).unwrap();
    let r = t.run("  .incchr \"big.pcx\"\n", AssembleOptions::default());
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.message.contains("too large")),
        "{:?}",
        r.diagnostics
    );
    fs::write(t.0.join("test.asm"), "  .db 1\n").unwrap();
    let request = AssembleRequest {
        input: "test.asm".into(),
        working_directory: t.0.clone(),
        include_paths: vec![],
        allowed_root: Some(t.0.clone()),
        options: AssembleOptions::default(),
        ..Default::default()
    };
    let r = nesasm_core::assemble_with_cancel(&request, &std::sync::atomic::AtomicBool::new(true));
    assert!(!r.success);
    assert!(r.diagnostics.iter().any(|d| d.code == "E_CANCELLED"));
}

#[test]
fn procedure_constants_are_not_relocated() {
    let t = Temp::new();
    let r = t.run(
        "  .bank 0\n  .org $8000\n  call bar\n  rts\n  .proc foo\n  nop\n  nop\n  rts\n  .endp\n  .proc bar\nCONST = 5\nSLOT .rs 1\n  lda #CONST\n  rts\n  .endp\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(r.symbols["CONST"].value, 5);
    assert_eq!(r.symbols["SLOT"].value, 0);
}

#[test]
fn parenthesized_operands_follow_csharp() {
    let t = Temp::new();
    let auto_zp = AssembleOptions {
        auto_zp: true,
        ..AssembleOptions::default()
    };
    let cases: [(&str, &AssembleOptions, &[u8]); 9] = [
        // Without AUTOZP parentheses only group expressions.
        (
            "  jmp ($1234)\n",
            &AssembleOptions::default(),
            &[0x4c, 0x34, 0x12],
        ),
        (
            "  lda ($10+1)*2,x\n",
            &AssembleOptions::default(),
            &[0xbd, 0x22, 0x00],
        ),
        // With AUTOZP `(zp,X)` and `(zp),Y` are indirect; `(zp)` is not.
        (
            "foo = $10\n  lda (foo)\n  sta (foo)\n",
            &auto_zp,
            &[0xa5, 0x10, 0x85, 0x10],
        ),
        ("foo = $10\n  lda (foo,x)\n", &auto_zp, &[0xa1, 0x10]),
        ("foo = $10\n  lda (foo),y\n", &auto_zp, &[0xb1, 0x10]),
        (
            "foo = $10\n  lda (foo), y++\n",
            &auto_zp,
            &[0xb1, 0x10, 0xc8],
        ),
        (
            "foo = $1000\n  lda (foo),y\n",
            &auto_zp,
            &[0xb9, 0x00, 0x10],
        ),
        ("  lda ($10+1)*2,x\n", &auto_zp, &[0xb5, 0x22]),
        ("  jmp ($1234)\n", &auto_zp, &[0x4c, 0x34, 0x12]),
    ];
    for (text, options, expected) in cases {
        let r = t.run(text, options.clone());
        assert!(r.success, "{text}: {:?}", r.diagnostics);
        assert_eq!(&r.binary[..expected.len()], expected, "{text}");
    }
    // The `(<zp),y` form needs AUTOZP; without it the operand is rejected.
    let r = t.run("  lda (<$12),y\n", AssembleOptions::default());
    assert!(!r.success);
}

#[test]
fn immediate_low_high_and_indirect_tags() {
    let t = Temp::new();
    let r = t.run(
        "zpv = $12\ntg = 3\n  lda #>zpv\n  lda #<$1234\n  lda #>$1234\n  lda [$10].tg\n  lda [$10], y\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(
        &r.binary[..12],
        &[
            0xa9, 0x00, 0xa9, 0x34, 0xa9, 0x12, 0xa0, 0x03, 0xb1, 0x10, 0xb1, 0x10
        ]
    );
    let r = t.run("  lda [$10].300\n", AssembleOptions::default());
    assert!(!r.success);
}

#[test]
fn label_bank_must_match_between_passes() {
    let t = Temp::new();
    let r = t.run(
        "  .bank 0\n  .org $c000\n  lda #BANK(foo)\n  .bank BNK\n  .org $8000\nfoo: nop\nBNK = 1\n",
        AssembleOptions::default(),
    );
    assert!(!r.success);
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.message.contains("Bank mismatch")),
        "{:?}",
        r.diagnostics
    );
}

#[test]
fn first_error_is_not_followed_by_missing_block_errors() {
    let t = Temp::new();
    let r = t.run(
        "  .if 1\n  .proc foo\n  lda #300\n  .endp\n  .endif\n",
        AssembleOptions::default(),
    );
    assert!(!r.success);
    assert_eq!(r.diagnostics.len(), 1, "{:?}", r.diagnostics);
}

#[test]
fn csharp_compatible_syntax() {
    let t = Temp::new();
    let r = t.run(
        concat!(
            "  .bank 0\n  .org $c000\n  .page 7\nlab:\n  nop\n  .page 4\n  .dw lab, *\n",
            "  .db 0x1F, 0X0a, %1100_0011\n",
            "  .db \"a\\\"b\"\n",
            "  lda.h $1234\n  lda.l #$1234\n",
            "  .db HIGH lab, LOW lab, HIGH lab+1, BANK lab, PAGE lab\n",
            "K = $1234\n  .db PAGE(K), PAGE K\n",
        ),
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(
        &r.binary[..23],
        &[
            0xea, 0x00, 0xe0, 0x01, 0x80, // .page
            0x1f, 0x0a, 0xc3, // literals
            0x61, 0x22, 0x62, // escaped quote
            0xad, 0x35, 0x12, 0xa9, 0x34, // .h / .l
            0xe0, 0x00, 0xe1, 0x00, 0x07, // keyword operators
            0xff, 0xff, // PAGE of a constant
        ]
    );
    assert_eq!(r.symbols["K"].page, None);
    assert!(!t.run("  .page 8\n", AssembleOptions::default()).success);
}

#[test]
fn column_one_words_are_labels() {
    let t = Temp::new();
    let r = t.run("nop\n\trts\n", AssembleOptions::default());
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..2], &[0x60, 0x00]);
    assert!(r.symbols.contains_key("nop"));
    assert!(
        !t.run(".bank 0\n  nop\n", AssembleOptions::default())
            .success
    );
    assert!(
        !t.run("m .macro\n  nop\n  .endm\nm\n", AssembleOptions::default())
            .success
    );
}

#[test]
fn line_errors_are_all_reported() {
    let t = Temp::new();
    let r = t.run(
        "  .bank 0\n  .org $8000\n  bne far\n  lda #$1234\n  lda missing\n  nop\n  .ds 200\nfar:\n  rts\n",
        AssembleOptions::default(),
    );
    assert!(!r.success);
    let lines: Vec<_> = r.diagnostics.iter().map(|d| d.location.line).collect();
    assert_eq!(lines, [3, 4, 5], "{:?}", r.diagnostics);
}

#[test]
fn listing_matches_csharp_layout() {
    let t = Temp::new();
    fs::write(t.0.join("data.bin"), vec![1; 100]).unwrap();
    let r = t.run(
        "  .list\n  .rsset $300\nv1 .rs 2\nsq .func 2\n  .zp\nzv: .ds 1\n  .code\n  .bank 0\n  .org $c000\n\tlda\t#1\n  .incbin \"data.bin\"\n  .ds 10\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    let listing = r.listing.unwrap();
    let line = |n: usize| {
        listing
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{n} ")))
            .unwrap_or_default()
            .to_string()
    };
    assert!(
        line(3).contains("0300") && !line(3).contains(":"),
        "{listing}"
    );
    assert!(!line(4).contains(":"), "{listing}");
    assert!(line(6).contains("--:0000"), "{listing}");
    assert!(line(10).ends_with("        lda     #1"), "{listing}");
    assert_eq!(listing.lines().count(), 12, "{listing}");
    // OPT l+ alone does not request a listing.
    let r = t.run("  .opt l+\n  nop\n", AssembleOptions::default());
    assert!(r.listing.is_none());
}

#[test]
fn diagnostics_point_at_opening_lines_and_plain_paths() {
    let t = Temp::new();
    let r = t.run("  nop\n  .if 1\n  nop\n", AssembleOptions::default());
    let d = &r.diagnostics[0];
    assert_eq!((d.message.as_str(), d.location.line), ("Missing ENDIF", 2));
    assert!(
        !d.location.file.to_string_lossy().starts_with(r"\\?\"),
        "{:?}",
        d.location.file
    );
    let r = t.run("  nop\nfoo .proc\n  rts\n", AssembleOptions::default());
    let d = &r.diagnostics[0];
    assert_eq!(
        (d.message.as_str(), d.location.line),
        ("Missing ENDP/ENDPROCGROUP", 2)
    );
}

#[test]
fn include_search_continues_past_rejected_directories() {
    let t = Temp::new();
    fs::create_dir_all(t.0.join("inc")).unwrap();
    fs::write(t.0.join("inc/part.asm"), "  .db 7\n").unwrap();
    fs::write(t.0.join("test.asm"), "  .include \"part.asm\"\n").unwrap();
    let request = AssembleRequest {
        input: "test.asm".into(),
        working_directory: t.0.clone(),
        include_paths: vec!["..".into(), "inc".into()],
        allowed_root: Some(t.0.clone()),
        options: AssembleOptions::default(),
        ..Default::default()
    };
    let r = nesasm_core::assemble(&request);
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(r.binary[0], 7);
    // `..` after a missing directory is rejected the same way on every OS.
    let r = t.run(
        "  .include \"missing/../part.asm\"\n",
        AssembleOptions::default(),
    );
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.message.contains("does not exist")),
        "{:?}",
        r.diagnostics
    );
}

#[test]
fn macro_arguments_are_substituted_once() {
    let t = Temp::new();
    // `\2` brought in by the first argument stays literal (the DB escape gives '2').
    let r = t.run(
        "pair .macro\n  .db \\1\n  .endm\n  pair \"a\\2\"\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..2], b"a2");
}

fn error_codes(r: &AssembleResult) -> Vec<String> {
    r.diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.code.to_string())
        .collect()
}

#[test]
fn nested_conditionals_select_one_branch() {
    let t = Temp::new();
    let r = t.run(
        concat!(
            "ON = 1\n",
            "  .if ON\n  .if 0\n  .db 1\n  .else\n  .db 2\n  .endif\n  .else\n  .db 3\n  .endif\n",
            "  .ifdef ON\n  .db 4\n  .endif\n",
            "  .ifndef ON\n  .db 5\n  .else\n  .db 6\n  .endif\n",
            "  .ifndef missing\n  .db 7\n  .endif\n",
        ),
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..4], &[2, 4, 6, 7]);
    for (text, code) in [
        ("  .else\n", "E_CONDITIONAL"),
        ("  .endif\n", "E_CONDITIONAL"),
        ("  .if 1\n  .else\n  .else\n  .endif\n", "E_CONDITIONAL"),
    ] {
        let r = t.run(text, AssembleOptions::default());
        assert_eq!(error_codes(&r), [code], "{text}");
    }
}

#[test]
fn rs_allocates_consecutive_constants() {
    let t = Temp::new();
    let r = t.run(
        "  .rsset $10\na .rs 2\nb .rs 1\nc .rs 4\n  .db a, b, c\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..3], &[0x10, 0x12, 0x13]);
    assert_eq!(r.symbols["c"].bank, nesasm_core::BankRef::Constant);
    assert!(
        !t.run("  .rsset $10000\n", AssembleOptions::default())
            .success
    );
}

#[test]
fn call_uses_a_trampoline_for_another_bank() {
    let t = Temp::new();
    let r = t.run(
        "  .bank 0\n  .org $c000\n  call far\nfar .proc\n  rts\n  .endp\n",
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    let far = &r.symbols["far"];
    assert_eq!(
        (far.bank, far.value),
        (nesasm_core::BankRef::Rom(1), 0xa000)
    );
    // JSR to the trampoline at $8000 in the bank after the procedures.
    assert_eq!(&r.binary[..3], &[0x20, 0x00, 0x80]);
    let call_bank = 2 * 0x2000;
    assert_eq!(
        r.binary[call_bank + 5],
        1,
        "trampoline selects the procedure bank"
    );
    assert_eq!(&r.binary[call_bank + 10..call_bank + 12], &[0x00, 0xa0]);
    assert_eq!(r.binary[0x2000], 0x60);
}

#[test]
fn fail_directive_reports_its_line() {
    let t = Temp::new();
    let r = t.run("  nop\n  .fail \"stop here\"\n", AssembleOptions::default());
    assert!(!r.success);
    let d = &r.diagnostics[0];
    assert_eq!((d.message.as_str(), d.location.line), ("\"stop here\"", 2));
}

#[test]
fn nesting_and_size_limits_are_errors() {
    let t = Temp::new();
    let r = t.run("  .include \"test.asm\"\n", AssembleOptions::default());
    assert_eq!(error_codes(&r), ["E_INCLUDE"]);
    assert!(r.diagnostics[0].message.contains("nesting limit"));
    let r = t.run(
        "loop .macro\n  loop\n  .endm\n  loop\n",
        AssembleOptions::default(),
    );
    assert_eq!(error_codes(&r), ["E_LIMIT"]);
    let r = t.run(
        &format!(";{}\n", "x".repeat(1024 * 1024)),
        AssembleOptions::default(),
    );
    assert_eq!(error_codes(&r), ["E_IO"]);
    assert!(r.diagnostics[0].message.contains("1 MiB"));
    let r = t.run("  .db 1, \\\n", AssembleOptions::default());
    assert_eq!(error_codes(&r), ["E_IO"]);
    assert!(r.diagnostics[0].message.contains("continuation"));
    let deep = "  .if 1\n".repeat(70);
    assert!(!t.run(&deep, AssembleOptions::default()).success);
}

#[test]
fn srec_records_have_valid_checksums() {
    let t = Temp::new();
    let r = t.run(
        "  .bank 0\n  .org $8000\nStart:\n  lda #1\n  .ds 40, $ea\n  rts\n",
        AssembleOptions {
            srec: true,
            ..AssembleOptions::default()
        },
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert!(r.header.is_empty());
    let srec = r.srec.unwrap();
    let mut records = 0;
    for line in srec.lines() {
        let bytes: Vec<u8> = (2..line.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&line[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(usize::from(bytes[0]), bytes.len() - 1, "{line}");
        let sum = bytes.iter().fold(0u8, |s, b| s.wrapping_add(*b));
        assert_eq!(sum, 0xff, "checksum of {line}");
        records += 1;
    }
    assert!(srec.lines().last().unwrap().starts_with("S8"));
    assert!(records >= 3);
}

#[test]
fn listing_level_is_validated_when_deserialized() {
    let parse = |json: &str| serde_json::from_str::<AssembleOptions>(json);
    assert!(parse(r#"{"list_level":3}"#).is_ok());
    assert!(parse(r#"{"list_level":4}"#).is_err());
    assert!(parse(r#"{"unknown":1}"#).is_err());
}

#[test]
fn directive_aliases_and_header_settings() {
    let t = Temp::new();
    let r = t.run(
        concat!(
            "  .ineschr 2\n  .inesmir 1\n  .inesmap 4\n",
            "VALUE .equ $1234\n",
            "emit .mac\n  .byte \\1\n  .endm\n",
            "  .bank 0\n  .org $8000\n",
            "  .word VALUE\n  emit 9\n",
            "  .autozp 1\n  lda $0010\n",
            "  .zp\nzp1: .ds 1\n  .bss\nram: .ds 2\n  .code\n  lda zp1\n",
            // DATA keeps its own cursor, which starts at offset 0 of the bank.
            "  .data\n  .db 5\n",
        ),
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.header[4..8], &[0, 2, 0x41, 0]);
    assert_eq!(&r.binary[..7], &[5, 0x12, 9, 0xa5, 0x10, 0xa5, 0x00]);
    assert_eq!(r.symbols["ram"].value, 0x200);
    assert_eq!(r.ram.bss_end, 0x202);
}

#[cfg(windows)]
#[test]
fn junction_escape_is_rejected() {
    let t = Temp::new();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.asm"), "  .db 1\n").unwrap();
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(t.0.join("link"))
        .arg(outside.path())
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
    let r = t.run(
        "  .include \"link/secret.asm\"\n",
        AssembleOptions::default(),
    );
    assert!(!r.success);
    assert!(
        r.diagnostics[0]
            .message
            .contains("outside the project root"),
        "{:?}",
        r.diagnostics
    );
    let ok = nesasm_core::assemble(&AssembleRequest {
        input: "test.asm".into(),
        working_directory: t.0.clone(),
        include_paths: vec![],
        allowed_root: None,
        options: AssembleOptions::default(),
        ..Default::default()
    });
    assert!(ok.success, "without a root the junction is followed");
}

#[test]
fn db_strings_keep_source_encoding_bytes() {
    let t = Temp::new();
    let r = t.run("  .db \"aあé\", 1\n", AssembleOptions::default());
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..7], &[0x61, 0xe3, 0x81, 0x82, 0xc3, 0xa9, 1]);
    let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("  .db \"aあ\\\"\", 1\n");
    fs::write(t.0.join("test.asm"), &sjis).unwrap();
    let r = nesasm_core::assemble(&AssembleRequest {
        input: "test.asm".into(),
        working_directory: t.0.clone(),
        include_paths: vec![],
        allowed_root: None,
        options: AssembleOptions {
            encoding: SourceEncoding::Sjis,
            ..AssembleOptions::default()
        },
        ..Default::default()
    });
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..5], &[0x61, 0x82, 0xa0, b'"', 1]);
}

fn memory_request(t: &Temp, files: &[(&str, &[u8])]) -> AssembleRequest {
    AssembleRequest {
        input: "main.asm".into(),
        working_directory: t.0.clone(),
        allowed_root: Some(t.0.clone()),
        files: files
            .iter()
            .map(|(name, data)| (PathBuf::from(name), data.to_vec()))
            .collect(),
        collect_lines: true,
        ..Default::default()
    }
}

#[test]
fn in_memory_files_take_precedence_and_stay_in_the_root() {
    let t = Temp::new();
    fs::write(t.0.join("main.asm"), "  .db 1\n").unwrap();
    fs::write(t.0.join("disk.bin"), [9, 9]).unwrap();
    let request = memory_request(
        &t,
        &[
            (
                "main.asm",
                b"  .include \"inc/part.asm\"\n  .incbin \"data.bin\"\n  .incbin \"disk.bin\"\n",
            ),
            ("inc/part.asm", b"  .db 2\n"),
            ("data.bin", &[3, 4]),
        ],
    );
    let r = nesasm_core::assemble(&request);
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..5], &[2, 3, 4, 9, 9]);
    assert!(r.dependencies.iter().any(|d| d.ends_with("part.asm")));
    // Nothing is written for in-memory files.
    assert!(!t.0.join("inc").exists());
    for name in ["../outside.asm", "/abs/main.asm"] {
        let mut request = memory_request(&t, &[("main.asm", b"  nop\n")]);
        request.files.insert(name.into(), b"  nop\n".to_vec());
        let r = nesasm_core::assemble(&request);
        assert!(!r.success, "{name}");
        assert!(
            r.diagnostics[0].message.contains("In-memory file"),
            "{:?}",
            r.diagnostics
        );
    }
}

#[test]
fn line_records_map_sources_to_addresses_and_bytes() {
    let t = Temp::new();
    let request = memory_request(
        &t,
        &[
            (
                "main.asm",
                b"emit .macro\n  lda #\\1\n  .endm\n  .zp\nvar: .ds 1\n  .code\n  .bank 0\n  .org $C000\nStart:\n  emit 5\n  .incbin \"big.bin\"\n",
            ),
            ("big.bin", &[7; 100]),
        ],
    );
    let r = nesasm_core::assemble(&request);
    assert!(r.success, "{:?}", r.diagnostics);
    let by_text = |text: &str| r.lines.iter().find(|l| l.text.trim() == text).unwrap();
    let var = by_text("var: .ds 1");
    assert_eq!((var.bank, var.address), (None, 0));
    let start = by_text("Start:");
    assert_eq!(
        (start.bank, start.address, start.size),
        (Some(0), 0xc000, 0)
    );
    let lda = by_text("lda #5");
    assert_eq!(
        (lda.location.line, lda.address, lda.bytes.as_slice()),
        (2, 0xc000, &[0xa9, 5][..])
    );
    assert_eq!(lda.called_from.as_ref().unwrap().line, 10);
    let incbin = by_text(".incbin \"big.bin\"");
    assert_eq!((incbin.size, incbin.bytes.len()), (100, 64));
    // Not collected unless requested.
    let mut request = request.clone();
    request.collect_lines = false;
    assert!(nesasm_core::assemble(&request).lines.is_empty());
}

#[test]
fn rom_inspection_decodes_header_vectors_and_code() {
    use nesasm_core::inspect::{Mirroring, Rom};
    let t = Temp::new();
    let r = t.run(
        concat!(
            "  .inesprg 1\n  .ineschr 1\n  .inesmap 1\n  .inesmir 1\n",
            "  .zp\nptr: .ds 2\n  .code\n",
            "  .bank 1\n  .org $E000\nReset:\n  lda [ptr],y\n  jmp Reset\n  .db $02\n",
            "Nmi: rti\n  .org $FFFA\n  .dw Nmi, Reset, 0\n",
            // CHR ROM: the bank after the two PRG banks.
            "  .bank 2\n  .org $0000\n  .db 0\n",
        ),
        AssembleOptions::default(),
    );
    assert!(r.success, "{:?}", r.diagnostics);
    let rom = Rom::from_assembly(&r);
    let header = rom.header.as_ref().unwrap();
    assert_eq!(
        (
            header.prg_16k,
            header.chr_8k,
            header.mapper,
            header.mirroring
        ),
        (1, 1, 1, Mirroring::Vertical)
    );
    assert_eq!(rom.bank_count(), 3);
    let vectors = rom.vectors();
    assert_eq!(
        (vectors[0].bank, vectors[0].reset, vectors[0].nmi),
        (1, 0xe000, 0xe006)
    );
    assert!(rom.warnings().is_empty(), "{:?}", rom.warnings());
    let code = rom.disassemble(1, 0xe000, 4, &r.symbols).unwrap();
    let text: Vec<&str> = code.iter().map(|l| l.instruction.as_str()).collect();
    assert_eq!(text, ["LDA [$00],Y", "JMP $E000", ".db $02", "RTI"]);
    assert_eq!(code[0].labels, ["Reset"]);
    assert_eq!(code[0].operand_symbols, ["ptr"]);
    assert_eq!(code[1].operand_symbols, ["Reset"]);
    assert_eq!(code[3].labels, ["Nmi"]);
    let dump = rom.hexdump(1, 0xfffa, 6).unwrap();
    assert_eq!(dump[0].bytes, "06 E0 00 E0 00 00");
    assert!(rom.disassemble(9, 0x8000, 1, &r.symbols).is_err());
    // The same ROM read back from a .nes file.
    let mut file = r.header.clone();
    file.extend(&r.binary);
    let from_file = Rom::from_file(&file).unwrap();
    assert_eq!(from_file.vectors()[0].reset, 0xe000);
    // A raw payload without header, and a header that does not match the data.
    assert!(Rom::from_file(&r.binary).unwrap().header.is_none());
    let short = Rom::from_file(&file[..16 + 8192]).unwrap();
    assert!(
        short.warnings().iter().any(|w| w.contains("declares")),
        "{:?}",
        short.warnings()
    );
}

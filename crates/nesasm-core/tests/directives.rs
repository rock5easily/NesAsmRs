mod common;

use common::Temp;
use nesasm_core::{AssembleOptions, BankRef, DiagnosticCode, Severity, SourceEncoding};

#[test]
fn data_strings_and_escapes() {
    let t = Temp::new();
    assert_eq!(
        t.bytes("  .db \"a\\n\\r\\t\\\\\\\"b\", 'c', 1\n", 9),
        [b'a', b'\n', b'\r', b'\t', b'\\', b'"', b'b', b'c', 1]
    );
    // Negative values down to -128 (DB) and -32768 (DW) are accepted.
    assert_eq!(
        t.bytes("  .db -128\n  .dw -32768, $ffff\n", 5),
        [0x80, 0x00, 0x80, 0xff, 0xff]
    );
    t.fails("  .db -129\n", "Data overflow");
    t.fails("  .dw $10000\n", "Data overflow");
    t.fails("  .dw -32769\n", "Data overflow");
    t.fails("  .db\n", "Missing data");
    t.fails("  .db \"abc\\\"\n", "Unterminated string");
    t.fails(
        "  .zp\n  .db 1\n",
        "Data emission not allowed in RAM section",
    );
    t.fails("  .bss\n  nop\n", "Instruction not allowed in RAM section");
}

#[test]
fn db_strings_must_be_encodable_as_sjis() {
    let t = Temp::new();
    let options = AssembleOptions {
        encoding: SourceEncoding::Sjis,
        ..AssembleOptions::default()
    };
    let (source, _, _) = encoding_rs::SHIFT_JIS.encode("  .db \"あ\"\n");
    t.write("test.asm", &source);
    let r = nesasm_core::assemble(&t.request(options.clone()));
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..2], &[0x82, 0xa0]);
    // An in-memory UTF-8 string with a character SJIS cannot encode.
    let mut request = t.request(options);
    request.files.insert(
        "test.asm".into(),
        "  .db \"\u{1F600}\"\n".as_bytes().to_vec(),
    );
    let r = nesasm_core::assemble(&request);
    assert!(!r.success);
}

#[test]
fn ds_reserves_ram_and_fills_rom() {
    let t = Temp::new();
    let r = t.run(concat!(
        "  .zp\nzp1: .ds 2\nzp2: .ds 1\n",
        "  .bss\nbss1: .ds $10\nbss2: .ds 1\n",
        "  .code\n  .org $8000\n  .ds 3, $ea\n  .ds 2\n  .db 1\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(r.symbols["zp2"].value, 2);
    assert_eq!(r.symbols["bss1"].value, 0x200);
    assert_eq!(r.symbols["bss2"].value, 0x210);
    assert_eq!((r.ram.zero_page_end, r.ram.bss_end), (3, 0x211));
    assert_eq!(&r.binary[..6], &[0xea, 0xea, 0xea, 0, 0, 1]);
    for (text, needle) in [
        ("  .ds\n", "DS expects count[, fill]"),
        ("  .ds 1, 2, 3\n", "DS expects count[, fill]"),
        ("  .ds 1, 256\n", "Fill out of range"),
        ("  .ds $1000000\n", "Allocation too large"),
        ("  .org $9fff\n  .ds 2\n", "DS out of range"),
        ("  .bss\n  .ds $601\n", "RAM allocation out of range"),
    ] {
        t.fails(text, needle);
    }
}

#[test]
fn org_ranges_depend_on_the_section() {
    let t = Temp::new();
    let r = t.run("  .zp\n  .org $f0\nzp: .ds 1\n  .bss\n  .org $700\nbss: .ds 1\n");
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(
        (r.symbols["zp"].value, r.symbols["bss"].value),
        (0xf0, 0x700)
    );
    for (text, needle) in [
        ("  .org $10000\n", "ORG out of range"),
        ("  .zp\n  .org $100\n", "Zero page ORG out of range"),
        ("  .bss\n  .org $800\n", "BSS ORG out of range"),
        (
            "p .proc\n  .org $8000\n  .endp\n",
            "ORG not allowed inside a procedure",
        ),
        ("  .org later\nlater:\n", "Undefined symbol"),
    ] {
        t.fails(text, needle);
    }
}

#[test]
fn org_label_and_page() {
    let t = Temp::new();
    let r = t.run("Start .org $8000\n  .page 5\nMoved:\n  nop\n");
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(r.symbols["Start"].value, 0x8000);
    // PAGE changes the CPU page but not the bank offset.
    assert_eq!(r.symbols["Moved"].value, 0xa000);
    assert_eq!(r.binary[0], 0xea);
    t.fails("  .page 8\n", "Invalid page index");
    t.fails(
        "p .proc\n  .page 5\n  .endp\n",
        "PAGE can not be changed in procs",
    );
}

#[test]
fn bank_arguments_and_names() {
    let t = Temp::new();
    let r = t.run(concat!(
        "  .bank 0, \"Main\"\n  .db 1\n",
        "  .bank 2, \"Extra\"\n  .db 2\n",
        // Names are compared without case; the last spelling is kept.
        "  .bank 0, \"MAIN\"\n  .db 3\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!((r.binary[0], r.binary[1], r.binary[0x4000]), (1, 3, 2));
    let names: Vec<_> = r.banks.iter().map(|b| (b.bank, b.name.clone())).collect();
    assert_eq!(
        names,
        [
            (0, Some("MAIN".into())),
            (1, None),
            (2, Some("Extra".into()))
        ]
    );
    for (text, needle) in [
        ("  .bank\n", "BANK expects one or two arguments"),
        ("  .bank 1, \"a\", 2\n", "BANK expects one or two arguments"),
        ("  .bank 128\n", "Bank out of range"),
        ("  .bank 0, Main\n", "Expected a quoted string"),
        (
            &format!("  .bank 0, \"{}\"\n", "n".repeat(64)),
            "Bank name too long",
        ),
        (
            "  .bank 0, \"a\"\n  .bank 0, \"b\"\n",
            "Different bank names not allowed",
        ),
        (
            "  .zp\n  .bank 1\n",
            "BANK not allowed in this section/procedure",
        ),
        ("p .proc\n  .bank 1\n  .endp\n", "BANK not allowed"),
    ] {
        t.fails(text, needle);
    }
}

#[test]
fn sections_keep_their_own_position() {
    let t = Temp::new();
    let r = t.run(concat!(
        "  .code\n  .org $8000\n  .db 1\n",
        "  .data\n  .org $8100\n  .db 2\n",
        "  .code\n  .db 3\n",
        "  .data\n  .db 4\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..2], &[1, 3]);
    assert_eq!(&r.binary[0x100..0x102], &[2, 4]);
    t.fails("  .code 1\n", "Unexpected section operand");
    t.fails(
        "p .proc\n  .data\n  .endp\n",
        "Section not allowed in procedure",
    );
}

#[test]
fn catbank_rs_and_autozp_operands() {
    let t = Temp::new();
    t.fails("  .catbank 128\n", "CATBANK out of range");
    t.fails("  .rsset $ffff\nA1 .rs 2\n", "RS out of range");
    t.fails("  .autozp 2\n", "AUTOZP must be 0 or 1");
    // AUTOZP selects zero page for small absolute operands and can be turned off.
    assert_eq!(
        t.bytes("  .autozp 1\n  lda $12\n  .autozp 0\n  lda $12\n", 5),
        [0xa5, 0x12, 0xad, 0x12, 0x00]
    );
}

#[test]
fn ines_header_fields_and_limits() {
    let t = Temp::new();
    let r = t.run("  .inesprg 2\n  .ineschr 1\n  .inesmap $a5\n  .inesmir 9\n  .db 0\n");
    assert!(r.success, "{:?}", r.diagnostics);
    // Mapper low nibble in byte 6 (above the mirroring bits), high nibble in byte 7.
    assert_eq!(&r.header[4..8], &[2, 1, 0x59, 0xa0]);
    for text in [
        "  .inesprg 65\n",
        "  .ineschr 65\n",
        "  .inesmap 256\n",
        "  .inesmir 16\n",
    ] {
        t.fails(text, "iNES parameter out of range");
    }
}

#[test]
fn raw_output_has_no_header() {
    let t = Temp::new();
    let r = t.run_with(
        "  .inesprg 1\n  .db 1\n",
        AssembleOptions {
            raw: true,
            ..AssembleOptions::default()
        },
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert!(r.header.is_empty());
    assert_eq!(r.binary[0], 1);
}

#[test]
fn listing_directives_and_opt() {
    let t = Temp::new();
    let r = t.run("  .list\n  .db 1\n  .nolist\n  .db 2\n  .list\n  .db 3\n");
    assert!(r.success, "{:?}", r.diagnostics);
    let listing = r.listing.unwrap();
    assert!(
        listing.contains(".db 1") && listing.contains(".db 3"),
        "{listing}"
    );
    assert!(!listing.contains(".db 2"), "{listing}");
    // OPT l- stops listing; m, w and o are accepted.
    let r = t.run("  .list\n  .opt l-\n  .db 1\n  .opt l+, m+, m-, w-, w+, o+\n  .db 2\n");
    assert!(r.success, "{:?}", r.diagnostics);
    let listing = r.listing.unwrap();
    assert!(
        !listing.contains(".db 1") && listing.contains(".db 2"),
        "{listing}"
    );
    for (text, needle) in [
        ("  .list 1\n", "Unexpected listing operand"),
        ("  .mlist x\n", "Unexpected listing operand"),
        ("  .opt l\n", "Invalid OPT flag"),
        ("  .opt z+\n", "Unknown OPT"),
    ] {
        t.fails(text, needle);
    }
}

#[test]
fn macro_listing_follows_mlist() {
    let t = Temp::new();
    let text = "m .macro\n  .db $5a\n  .endm\n  .list\n  m\n  .mlist\n  m\n";
    let r = t.run(text);
    assert!(r.success, "{:?}", r.diagnostics);
    let listing = r.listing.unwrap();
    // Only the expansion after .MLIST lists the body line.
    assert_eq!(listing.matches(".db $5a").count(), 1, "{listing}");
    assert!(
        listing.find(".db $5a") > listing.find(".mlist"),
        "{listing}"
    );
}

#[test]
fn opt_w_enables_bank_overflow_warnings() {
    let t = Temp::new();
    t.write("big.bin", vec![1u8; 0x2010]);
    // INCBIN may cross into the next bank; OPT w+ reports it.
    let r = t.run("  .incbin \"big.bin\"\n");
    assert!(r.success, "{:?}", r.diagnostics);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    let r = t.run("  .opt w+\n  .incbin \"big.bin\"\n");
    assert!(r.success, "{:?}", r.diagnostics);
    let warning = &r.diagnostics[0];
    assert_eq!(
        (warning.severity, warning.code, warning.location.line),
        (Severity::Warning, DiagnosticCode::BankOverflow, 2)
    );
    assert_eq!(warning.message, "Bank overflow by 16 bytes");
    let r = t.run_with(
        "  .opt w+\n  .incbin \"big.bin\"\n",
        AssembleOptions {
            warning_disabled: true,
            ..AssembleOptions::default()
        },
    );
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
}

#[test]
fn incbin_arguments_and_ranges() {
    let t = Temp::new();
    t.write("data.bin", [1, 2, 3, 4]);
    assert_eq!(
        t.bytes("  .incbin \"data.bin\"\n  .incbin \"data.bin\", 3\n", 5),
        [1, 2, 3, 4, 4]
    );
    for (text, needle) in [
        ("  .incbin\n", "INCBIN expects file[, offset[, size]]"),
        (
            "  .incbin \"data.bin\",0,1,2\n",
            "INCBIN expects file[, offset[, size]]",
        ),
        ("  .incbin data.bin\n", "Expected a quoted string"),
        ("  .incbin \"missing.bin\"\n", "missing.bin"),
        ("  .incbin \"data.bin\", 5\n", "INCBIN offset out of range"),
        (
            "  .incbin \"data.bin\", 2, 3\n",
            "INCBIN range out of bounds",
        ),
        ("  .incbin \"data.bin\", undefined\n", "Undefined symbol"),
    ] {
        t.fails(text, needle);
    }
    t.write("big.bin", vec![0u8; 0x2001]);
    t.fails(
        "p .proc\n  .incbin \"big.bin\"\n  .endp\n",
        "Procedure exceeds 8 KiB",
    );
}

#[test]
fn incchr_arguments() {
    let t = Temp::new();
    for (text, needle) in [
        ("  .incchr\n", "INCCHR expects a PCX file"),
        ("  .incchr chr.pcx\n", "Expected a quoted string"),
        ("  .incchr \"missing.pcx\"\n", "missing.pcx"),
    ] {
        t.fails(text, needle);
    }
    t.write("bad.pcx", [0u8; 4]);
    assert!(!t.run("  .incchr \"bad.pcx\"\n").success);
}

#[test]
fn defchr_needs_eight_rows() {
    let t = Temp::new();
    t.fails("  .defchr 0,0,0\n", "DEFCHR expects eight rows");
    t.fails("  .defchr 0,0,0,0,0,0,0,missing\n", "Undefined symbol");
}

#[test]
fn equ_and_symbol_definitions() {
    let t = Temp::new();
    let r = t.run("A1 .equ 5\nA2 = A1+1\nA2 = 6\nlabel:\n  .db A1, A2\n");
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..2], &[5, 6]);
    assert_eq!(r.symbols["A2"].bank, BankRef::Constant);
    assert_eq!(r.symbols["A2"].page, None);
    t.fails("  .equ 5\n", "EQU requires a label");
    t.fails("A1 = 1\nA1 = 2\n", "Symbol 'A1' is multiply defined");
    t.fails("L:\n  nop\nL:\n  nop\n", "Symbol 'L' is multiply defined");
    // Redefining a symbol with the same value is allowed.
    assert!(t.run("L:\nL:\n  nop\n").success);
    // Labels start a scope for local labels.
    let r = t.run("  .org $8000\nOne:\n.x: nop\nTwo:\n.x: nop\n");
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(r.symbols["One.x"].value, 0x8000);
    assert_eq!(r.symbols["Two.x"].value, 0x8001);
}

#[test]
fn reserved_symbols_cannot_be_redefined() {
    let t = Temp::new();
    let r = t.run("  .db 0\n");
    let reserved: Vec<_> = r
        .symbols
        .values()
        .filter(|s| s.location.line == 0)
        .map(|s| s.name.clone())
        .collect();
    assert!(!reserved.is_empty());
    let name = &reserved[0];
    t.fails(
        &format!("{name}:\n  nop\n"),
        "Reserved symbol cannot be redefined",
    );
}

#[test]
fn public_needs_a_local_label() {
    let t = Temp::new();
    for (text, needle) in [
        ("  .public\n", "PUBLIC requires a local label"),
        ("Global .public\n", "PUBLIC requires a local label"),
        ("Global:\n.loc .public 1\n", "Unexpected PUBLIC operand"),
    ] {
        t.fails(text, needle);
    }
}

#[test]
fn regions_report_their_size() {
    let t = Temp::new();
    let r = t.run(concat!(
        "  .org $8000\n  .beginregion \"r\"\n  .ds 10\n  .endregion \"r\"\n",
        "  .db REGIONSIZE(\"r\")\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(r.regions["r"].size, Some(10));
    assert_eq!(r.binary[10], 10);
    for (text, needle) in [
        ("  .beginregion \"\"\n", "Invalid region name"),
        (
            &format!("  .beginregion \"{}\"\n", "r".repeat(65)),
            "Invalid region name",
        ),
        (
            "  .beginregion \"r\"\n  .beginregion \"r\"\n",
            "Region endpoint multiply defined",
        ),
        ("  .beginregion r\n", "Expected a quoted string"),
    ] {
        t.fails(text, needle);
    }
}

#[test]
fn comments_continuations_and_byte_order_mark() {
    let t = Temp::new();
    assert_eq!(
        t.bytes(
            concat!(
                "\u{feff}; comment\n",
                "* a comment in column 1\n",
                "  .db 1, \\ ; continued\n",
                "      2, \\\n",
                "      3 ; done\n",
                "  .db 4\n",
            ),
            4
        ),
        [1, 2, 3, 4]
    );
    // Line numbers stay those of the source file.
    let r = t.run("  .db 1, \\\n    2\n  lda #300\n");
    assert_eq!(r.diagnostics[0].location.line, 3);
}

#[test]
fn invalid_sjis_source() {
    let t = Temp::new();
    t.write("test.asm", [b' ', 0x82, 0x20, b'\n']);
    let r = nesasm_core::assemble(&t.request(AssembleOptions {
        encoding: SourceEncoding::Sjis,
        ..AssembleOptions::default()
    }));
    assert_eq!(common::messages(&r), ["Invalid SJIS source"]);
}

#[test]
fn stray_block_ends_and_fail() {
    let t = Temp::new();
    t.fails("  .endm\n", "Unexpected ENDM");
    t.fails("  .fail\n", "Assembly failed");
    t.fails("  .endp\n", "Unexpected procedure end");
    t.fails(
        "label: extra 1\n",
        "Unknown instruction or addressing mode 'EXTRA'",
    );
}

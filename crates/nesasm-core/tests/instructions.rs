mod common;

use common::Temp;
use nesasm_core::inspect::Rom;
use std::collections::BTreeMap;

/// Every opcode the assembler knows disassembles to text that assembles back
/// to the same bytes, which checks the encoder, the decoder and their syntax.
#[test]
fn every_opcode_round_trips_through_the_disassembler() {
    let t = Temp::new();
    let mut known = 0;
    let mut cmos = Vec::new();
    let mut expected = Vec::new();
    for code in 0..=255u8 {
        let rom = Rom::from_file(&[code, 0x34, 0x12]).unwrap();
        let line = &rom.disassemble(0, 0x8000, 1, &BTreeMap::new()).unwrap()[0];
        if line.instruction.starts_with(".db") {
            continue;
        }
        known += 1;
        if line.cmos_only {
            cmos.push(code);
        }
        let length = line.bytes.split(' ').count();
        // Assembled at $8000 like the disassembly, so branches keep their offset.
        let r = t.run(&format!("  .org $8000\n  {}\n", line.instruction));
        assert!(
            r.success,
            "{code:02X} {}: {:?}",
            line.instruction, r.diagnostics
        );
        assert_eq!(
            &r.binary[..length],
            &[code, 0x34, 0x12][..length],
            "{code:02X} {}",
            line.instruction
        );
        expected.push(code);
    }
    // 151 NMOS 6502 opcodes plus the 65C02 forms of the legacy assembler.
    assert_eq!(known, 151 + cmos.len(), "{expected:02X?}");
    assert_eq!(
        cmos,
        [
            0x12, 0x1a, 0x32, 0x34, 0x3a, 0x3c, 0x52, 0x72, 0x7c, 0x89, 0x92, 0xb2, 0xd2, 0xf2
        ]
    );
}

#[test]
fn byte_selection_extensions() {
    let t = Temp::new();
    assert_eq!(
        t.bytes(
            concat!(
                "  lda.l $1234\n  lda.h $1234\n  lda.h #$1234\n",
                "  lda low_byte $1234\n  lda high_byte $1234\n",
                // ,X++ appends INX; .h does not move it to the high byte.
                "  lda $1234,x++\n  lda.h $1234,y++\n",
            ),
            21
        ),
        [
            0xad, 0x34, 0x12, 0xad, 0x35, 0x12, 0xa9, 0x12, 0xad, 0x34, 0x12, 0xad, 0x35, 0x12,
            0xbd, 0x34, 0x12, 0xe8, 0xb9, 0x34, 0x12,
        ]
    );
}

#[test]
fn indirect_forms() {
    let t = Temp::new();
    assert_eq!(
        t.bytes(
            "  lda [$12]\n  lda [$12,x]\n  lda [$12],y++\n  jmp [$1234]\n  jmp [$1234,x]\n  lda [$12].5\n",
            17
        ),
        [
            0xb2, 0x12, 0xa1, 0x12, 0xb1, 0x12, 0xc8, 0x6c, 0x34, 0x12, 0x7c, 0x34, 0x12, 0xa0,
            0x05, 0xb1, 0x12,
        ]
    );
    let r = t.run_with(
        "  jmp ($1234,x)\n",
        nesasm_core::AssembleOptions {
            auto_zp: true,
            ..Default::default()
        },
    );
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..3], &[0x7c, 0x34, 0x12]);
}

#[test]
fn zero_page_selection() {
    let t = Temp::new();
    assert_eq!(
        t.bytes(
            // `<` forces zero page, `>` keeps absolute even with AUTOZP.
            "  lda <$12\n  ldx <$12,y\n  .autozp 1\n  lda $12,x\n  lda >$12\n  stx $12,y\n",
            11
        ),
        [
            0xa5, 0x12, 0xb6, 0x12, 0xb5, 0x12, 0xad, 0x12, 0x00, 0x96, 0x12
        ]
    );
}

#[test]
fn instruction_errors() {
    let t = Temp::new();
    for (text, needle) in [
        (
            "  lda.x #1\n",
            "Unknown instruction extension; use .l or .h",
        ),
        ("  nop.l\n", "Instruction extension not supported"),
        ("  bne.h $e000\n", "Instruction extension not supported"),
        ("  nop 1\n", "Unexpected operand"),
        (
            "  lda.l low_byte $1234\n",
            "Duplicate instruction extension",
        ),
        ("  lda A\n", "Invalid accumulator mode"),
        ("  jmp <$12\n", "Invalid zero page mode"),
        (
            "  stx $1234,x\n",
            "Unknown instruction or addressing mode 'STX'",
        ),
        (
            "  foo $12\n",
            "Unknown instruction or addressing mode 'FOO'",
        ),
        (
            "  lda.l [$12]\n",
            "Instruction extension not supported in indirect modes",
        ),
        ("  lda [$12\n", "Missing indirect delimiter"),
        ("  lda [$12],x\n", "Invalid indirect operand"),
        ("  lda [$12],y+\n", "Invalid indirect operand"),
        ("  lda $10000\n", "Operand size error"),
        ("  lda #$100\n", "Operand size error"),
        ("  lda <$100\n", "Operand size error"),
        ("  bne missing\n", "Undefined symbol 'missing'"),
    ] {
        t.fails(text, needle);
    }
}

#[test]
fn negative_immediates_are_bytes() {
    let t = Temp::new();
    assert_eq!(
        t.bytes("  lda #-1\n  ldx #-128\n", 4),
        [0xa9, 0xff, 0xa2, 0x80]
    );
    t.fails("  lda #-257\n", "Operand size error");
}

#[test]
fn operand_edge_cases() {
    let t = Temp::new();
    assert_eq!(
        t.bytes(
            // AUTOZP keeps absolute mode when no zero page form exists, and
            // `(..)` operands may nest parentheses or quote a `)`.
            "  .autozp 1\n  jmp $12\n  lda ((1)),y\n  lda (')'),y\n",
            7
        ),
        [0x4c, 0x12, 0x00, 0xb1, 0x01, 0xb1, 0x29]
    );
    for (text, needle) in [
        ("  lda [$12)]\n", "Unbalanced parentheses"),
        ("  lda [$12].missing\n", "Undefined symbol 'missing'"),
        ("  .autozp 1\n  lda (1\n", "Expected ')'"),
    ] {
        t.fails(text, needle);
    }
}

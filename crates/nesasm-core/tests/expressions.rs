mod common;

use common::Temp;

#[test]
fn binary_operators() {
    let t = Temp::new();
    assert_eq!(
        t.bytes(
            "  .db 6|3, 6^3, 6&3, 1<<4, $80>>3, 7%3, 7/2, 10-3-2, 2*3%4\n",
            9
        ),
        [7, 5, 2, 16, 16, 1, 3, 5, 2]
    );
    // Comparisons are signed and give 0 or 1.
    assert_eq!(
        t.bytes(
            "  .db 1=1, 1==1, 1!=2, 1<>1, 2<3, 3<=3, 4>3, 3>=4, -1<0, -1>=0\n",
            10
        ),
        [1, 1, 1, 0, 1, 1, 1, 0, 1, 0]
    );
    // Shifts right are arithmetic; division and remainder truncate toward zero.
    assert_eq!(
        t.bytes("  .dw -8>>1, -7/2, -7%2\n", 6),
        [0xfc, 0xff, 0xfd, 0xff, 0xff, 0xff]
    );
}

#[test]
fn operator_precedence() {
    let t = Temp::new();
    assert_eq!(
        t.bytes(
            // & > ^ > |, + > <<, comparison > equality > & (as in C).
            "  .db 1|2^3&4, 1+2<<1, 1<2=1, 2+3*4, (2+3)*4, 6&3=2, (6&3)=2\n",
            7
        ),
        [3, 6, 1, 14, 20, 0, 1]
    );
}

#[test]
fn unary_operators_and_byte_selection() {
    let t = Temp::new();
    assert_eq!(
        t.bytes(
            "  .db ~0&$ff, !0, !5, -1&$ff, +5, <$1234, >$1234, -(2+3)&$ff\n",
            8
        ),
        [0xff, 1, 0, 0xff, 5, 0x34, 0x12, 0xfb]
    );
    // LOW/HIGH are functions and prefix operators that bind tighter than `+`.
    assert_eq!(
        t.bytes(
            "  .db LOW($1234), HIGH($1234), low($1234), LOW $1234+1, HIGH $1234+1\n",
            5
        ),
        [0x34, 0x12, 0x34, 0x35, 0x13]
    );
}

#[test]
fn numeric_and_character_literals() {
    let t = Temp::new();
    assert_eq!(
        t.bytes("  .db $ff, $FF, %1010_0101, 0x1F, 0X10, 10, 'A', ' '\n", 8),
        [255, 255, 0xa5, 0x1f, 0x10, 10, b'A', b' ']
    );
    // `%` is a binary literal where an operand is expected, modulo after one.
    assert_eq!(t.bytes("  .db %11, 7 % 3, (5)%3\n", 3), [3, 1, 2]);
}

#[test]
fn program_counter() {
    let t = Temp::new();
    assert_eq!(
        t.bytes("  .org $8000\n  .dw *, *+2\n  .dw *\n", 6),
        [0x00, 0x80, 0x02, 0x80, 0x04, 0x80]
    );
}

#[test]
fn expression_functions() {
    let t = Temp::new();
    assert_eq!(
        t.bytes(
            "SQ .func \\1*\\1\nADD3 .func \\1+\\2+\\3\n  .db SQ(3), ADD3(1,2,SQ(2)), SQ(1+2)\n",
            3
        ),
        [9, 7, 9]
    );
    t.fails("F .func 1\nF .func 2\n", "Duplicate function");
    t.fails("  .func 1\n", "FUNC requires a label");
    let r = t.fails("F .func F(\\1)\nX = F(1)\n", "recursion limit");
    assert_eq!(common::error_codes(&r), ["E_ASSEMBLY"]);
}

#[test]
fn symbol_functions_and_keywords() {
    let t = Temp::new();
    let r = t.run(concat!(
        "C = 5\n",
        "  .bank 3\n  .org $c000\n",
        // SIZEOF covers the following unlabelled DB lines, up to the instruction.
        "Data: .db 1,2,3\n  .db 4\n  nop\n",
        "  .db DEFINED(Data), DEFINED(nothing), DEFINED Data\n",
        "  .db BANK(Data), BANK Data, PAGE(Data), SIZEOF(Data)\n",
        // Constants have no page: PAGE() is -1, as in C#.
        "  .db PAGE(C)&$ff\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    let bank = &r.binary[3 * 0x2000..];
    assert_eq!(&bank[5..13], &[1, 0, 1, 3, 3, 6, 4, 0xff]);
}

#[test]
fn local_symbols_need_public_outside_their_scope() {
    let t = Temp::new();
    assert_eq!(
        t.bytes("  .org $8000\nGlobal:\n.loc:\n  .dw .loc\n", 2),
        [0x00, 0x80]
    );
    t.fails(
        "  .org $8000\nGlobal:\n.loc:\nOther:\n  .dw Global.loc\n",
        "Undefined symbol 'Global.loc'",
    );
}

#[test]
fn expression_errors() {
    let t = Temp::new();
    for (text, needle) in [
        ("X = 'a\n", "Unterminated literal"),
        ("X = 'ab'\n", "Character literal must be one byte"),
        ("X = 1 @ 2\n", "Invalid expression character '@'"),
        ("X = 1 2\n", "Unexpected expression suffix"),
        ("X = (1\n", "Expected ')'"),
        ("X = 1 +\n", "Expected expression"),
        ("X = 1/0\n", "Division by zero"),
        ("X = 1%0\n", "Division by zero"),
        ("X = $80000000/-1\n", "Division overflow"),
        ("X = $80000000%-1\n", "Division overflow"),
        ("X = $100000000\n", "Invalid numeric literal"),
        ("X = $\n", "Invalid numeric literal"),
        ("X = FOO(1)\n", "Unknown function 'FOO'"),
        ("X = LOW(1,2)\n", "Expected one argument"),
        ("X = BANK(1)\n", "BANK requires a symbol"),
        ("X = BANK(missing)\n", "Undefined symbol"),
        ("X = missing\n", "Undefined symbol 'missing'"),
        ("C = 1\nX = BANK(C)\n", "No BANK attribute"),
        ("C = 1\nX = SIZEOF(C)\n", "No SIZEOF attribute"),
        ("L:\nX = VRAM(L)\n", "No VRAM attribute"),
        ("L:\nX = PAL(L)\n", "No PAL attribute"),
        ("X = REGIONSIZE(1)\n", "REGIONSIZE requires a string"),
        (
            "X = REGIONSIZE(\"none\")\n",
            "Region is undefined or incomplete",
        ),
    ] {
        t.fails(text, needle);
    }
}

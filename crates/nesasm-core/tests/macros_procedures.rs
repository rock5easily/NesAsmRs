mod common;

use common::{Temp, error_codes};
use nesasm_core::BankRef;

#[test]
fn macro_unique_labels_and_names_without_case() {
    let t = Temp::new();
    let r = t.run(concat!(
        "  .org $8000\n",
        "Skip .macro\n  bne .s\\@\n  nop\n.s\\@:\n  .endm\n",
        "Start:\n  skip\n  SKIP\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..6], &[0xd0, 0x01, 0xea, 0xd0, 0x01, 0xea]);
    assert_eq!(r.symbols["Start.s00001"].value, 0x8003);
    assert_eq!(r.symbols["Start.s00002"].value, 0x8006);
}

#[test]
fn macro_argument_kinds() {
    let t = Temp::new();
    let r = t.run(concat!(
        "C = 1\n",
        "kinds .macro\n  .db \\?1, \\?2, \\?3, \\?4, \\?5, \\?6, \\?7, \\?8, \\?9\n  .endm\n",
        "Lbl:\n  kinds x, #1, C, [$10], \"s\", Lbl, 1+2, a\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    // register, immediate, constant, indirect, string, label, expression, register, none
    assert_eq!(&r.binary[..9], &[1, 2, 3, 4, 5, 6, 3, 1, 0]);
}

#[test]
fn macro_arguments_with_braces_and_index_registers() {
    let t = Temp::new();
    let r = t.run(concat!(
        "load .macro\n  lda \\1\n  .db \\#\n  .endm\n",
        "bytes .macro\n  .db \\1\n  .endm\n",
        // `,x` joins the previous argument; braces keep commas in one argument.
        "  load $1234,x\n  bytes {1,2,3}\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(&r.binary[..7], &[0xbd, 0x34, 0x12, 1, 1, 2, 3]);
}

#[test]
fn macro_call_labels_are_defined() {
    let t = Temp::new();
    let r = t.run("m .macro\n  nop\n  .endm\n  .org $8000\n  nop\nHere: m\n");
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(r.symbols["Here"].value, 0x8001);
}

#[test]
fn macro_definition_errors() {
    let t = Temp::new();
    for (text, code, needle) in [
        (
            "1bad .macro\n  nop\n  .endm\n",
            "E_MACRO",
            "Invalid macro name",
        ),
        (
            "m .macro\n  nop\n  .endm\nM .macro\n  nop\n  .endm\n",
            "E_MACRO",
            "Invalid or duplicate macro",
        ),
        (
            "  .macro\n  nop\n  .endm\n",
            "E_MACRO",
            "Invalid or duplicate macro",
        ),
        (
            "m .macro\n  nop\n",
            "E_MACRO",
            "Incomplete MACRO definition (started at line 1)",
        ),
        (
            "m .macro\n  .db \\1\n  .endm\n  m 1,2,3,4,5,6,7,8,9,10\n",
            "E_MACRO",
            "Maximum nine macro arguments",
        ),
        (
            "m .macro\n  nop\n  .endm\n  m 1, , 2\n",
            "E_MACRO",
            "Missing argument",
        ),
        (
            "m .macro\n  nop\n  .endm\n  m (1\n",
            "E_MACRO",
            "Unterminated string or parentheses",
        ),
        (
            "m .macro\n  nop\n  .endm\nL:\n  nop\nL: m\n",
            "E_SYMBOL",
            "multiply defined",
        ),
    ] {
        let r = t.fails(text, needle);
        assert_eq!(error_codes(&r)[0], code, "{text}");
    }
}

#[test]
fn procedures_are_relocated_after_the_code() {
    let t = Temp::new();
    let r = t.run(concat!(
        "  .bank 0\n  .org $e000\n",
        "  call one\n  call two\n  call one\n  jsr three\n",
        "one .proc\n  call two\n  rts\n  .endp\n",
        "two .proc\n  rts\n  .endp\n",
        "  .procgroup\nthree .proc\n  rts\n  .endp\nfour .proc\n  rts\n  .endp\n  .endprocgroup\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    // Procedures go to the bank after the code, at $A000, in definition order.
    let at = |name: &str| (r.symbols[name].bank, r.symbols[name].value);
    assert_eq!(at("one"), (BankRef::Rom(1), 0xa000));
    assert_eq!(at("two"), (BankRef::Rom(1), 0xa004));
    assert_eq!(at("three"), (BankRef::Rom(1), 0xa005));
    assert_eq!(at("four"), (BankRef::Rom(1), 0xa006));
    // Calls from another bank share one trampoline per procedure.
    assert_eq!(
        &r.binary[..12],
        &[
            0x20, 0x00, 0x80, 0x20, 0x12, 0x80, 0x20, 0x00, 0x80, 0x20, 0x05, 0xa0
        ]
    );
    // A call within the procedure bank is a plain JSR.
    assert_eq!(&r.binary[0x2000..0x2004], &[0x20, 0x04, 0xa0, 0x60]);
    assert_eq!(r.symbols["_call_bank"].value, 2);
}

#[test]
fn call_to_a_plain_label_is_a_jsr() {
    let t = Temp::new();
    assert_eq!(
        t.bytes("  .org $8000\nsub:\n  rts\n  call sub\n", 4),
        [0x60, 0x20, 0x00, 0x80]
    );
    t.fails("  call missing\n", "Undefined symbol");
}

#[test]
fn procedure_errors() {
    let t = Temp::new();
    for (text, needle) in [
        (
            "  .data\np .proc\n  .endp\n",
            "Procedure requires CODE section",
        ),
        (
            "a .proc\nb .proc\n  .endp\n  .endp\n",
            "Cannot nest procedures/groups",
        ),
        (
            "  .procgroup\n  .procgroup\n  .endprocgroup\n  .endprocgroup\n",
            "Cannot nest procedures/groups",
        ),
        ("  .proc\n  .endp\n", "Invalid procedure name"),
        (".loc .proc\n  .endp\n", "Invalid procedure name"),
        (
            "p .proc\n  .endp\np .proc\n  .endp\n",
            "Duplicate procedure",
        ),
        ("g .procgroup\n  .endp\n", "Mismatched procedure end"),
        ("p .proc\n  .endprocgroup\n", "Mismatched procedure end"),
        ("  .endprocgroup\n", "Unexpected procedure end"),
    ] {
        t.fails(text, needle);
    }
}

#[test]
fn procedures_need_rom_space() {
    let t = Temp::new();
    let r = t.fails(
        "  .bank 127\n  nop\np .proc\n  rts\n  .endp\n",
        "Not enough ROM space for procedures",
    );
    // Reported at the procedure that does not fit.
    assert_eq!(error_codes(&r)[0], "E_PROC");
    assert_eq!(r.diagnostics[0].location.line, 3);
    // The procedure fits in bank 127, but the trampoline bank would be 128.
    t.fails(
        "  .bank 126\n  call p\np .proc\n  rts\n  .endp\n",
        "Call bank exceeds ROM limit",
    );
}

#[test]
fn trampoline_bank_overflows_after_455_procedures() {
    let t = Temp::new();
    let count = 0x2000 / 18 + 1;
    let mut text = String::new();
    for i in 0..count {
        text.push_str(&format!("  call p{i}\n"));
    }
    for i in 0..count {
        text.push_str(&format!("p{i} .proc\n  rts\n  .endp\n"));
    }
    t.fails(&text, "Call bank overflow");
    // One procedure fewer fits.
    let fits = text
        .replace(&format!("  call p{}\n", count - 1), "")
        .replace(&format!("p{} .proc\n  rts\n  .endp\n", count - 1), "");
    let r = t.run(&fits);
    assert!(r.success, "{:?}", r.diagnostics.first());
}

#[test]
fn macro_local_constants_and_literal_backslashes() {
    let t = Temp::new();
    let r = t.run(concat!(
        "G:\n.c = 1\n",
        "k .macro\n  .db \\?1, \"\\?x\\n\"\n  .endm\n",
        "  k .c\n",
    ));
    assert!(r.success, "{:?}", r.diagnostics);
    // A local constant is a constant argument; `\?x` and `\n` are not substituted.
    assert_eq!(&r.binary[..4], &[3, b'?', b'x', b'\n']);
}

#[test]
fn procedure_labels_and_size() {
    let t = Temp::new();
    t.fails(
        "p:\n  nop\np .proc\n  rts\n  .endp\n",
        "Symbol 'p' is multiply defined",
    );
    t.fails(
        "p .proc\n  .ds $2000\n  .db 1\n  .endp\n",
        "Procedure exceeds 8 KiB",
    );
}

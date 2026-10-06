"""Differential checks against the unmodified C# core (Windows only).

Usage: python tools/compat/compare.py --oracle path.exe --rust target/debug/nesasm.exe
The oracle and Rust run in separate processes with identical input/options/CWD.
All generated inputs and outputs stay under target/compat/cases.
"""
import argparse
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]


def compare(oracle, rust, fixture, output, flags=()):
    output.mkdir(parents=True, exist_ok=True)
    old_file = output / "result.json"
    old_file.unlink(missing_ok=True)
    subprocess.run([str(oracle), str(fixture), str(output), *flags], check=True, timeout=30)
    if not old_file.exists():
        raise AssertionError("C# oracle exited without reporting a result")
    old = json.loads(old_file.read_text(encoding="utf-8-sig"))
    rust_flags = []
    mapping = {"--sjis": ["-e", "SJIS"], "--raw": ["-raw"], "--autozp": ["-autozp"],
               "--srec": ["-srec"], "--list3": ["-l3"], "--list1": ["-l1"],
               "--list0": ["-l0"], "--mlist": ["-m"]}
    for flag in flags:
        rust_flags.extend(mapping[flag])
    proc = subprocess.run([str(rust), "--json", "--check", *rust_flags, str(fixture)],
                          cwd=fixture.parent, capture_output=True, text=True, encoding="utf-8", timeout=30)
    if not proc.stdout:
        raise AssertionError(f"Rust did not report a result: {proc.stderr}")
    new = json.loads(proc.stdout)
    (output / "rust.json").write_text(json.dumps(new), encoding="utf-8")
    if old["success"] != new["success"]:
        raise AssertionError(f"success differs\nC#: {old['log']}\nRust: {new['diagnostics']}")
    if proc.returncode != (0 if new["success"] else 1):
        raise AssertionError("Unexpected Rust exit status")
    if not old["success"]:
        return
    for field in ("binary", "map", "header"):
        if old[field] != new[field]:
            a, b = old[field], new[field]
            index = next((i for i, (x, y) in enumerate(zip(a, b)) if x != y), min(len(a), len(b)))
            raise AssertionError(f"{field} differs at {index:#x}; lengths {len(a)}/{len(b)}")
    for name, symbol in new["symbols"].items():
        if symbol["location"]["line"] == 0:
            continue
        if name not in old["symbols"]:
            raise AssertionError(f"Missing C# symbol {name}")
        for field in ("value", "bank", "size"):
            if old["symbols"][name][field] != symbol[field]:
                raise AssertionError(f"Symbol {name} {field}: {old['symbols'][name][field]} != {symbol[field]}")
    for name, region in old["regions"].items():
        if new["regions"].get(name, {}).get("size") != region["size"]:
            raise AssertionError(f"Region {name} differs")
    def normalize(text):
        return (text or "").replace("\r\n", "\n").replace(str(fixture), "<input>")
    if normalize(old["srec"]) != normalize(new["srec"]):
        raise AssertionError("S-record differs")
    if normalize(old["listing"]) != normalize(new["listing"]):
        import difflib
        diff = "\n".join(difflib.unified_diff(normalize(old["listing"]).splitlines(), normalize(new["listing"]).splitlines()))
        raise AssertionError(f"Listing differs:\n{diff[:2500]}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--oracle", required=True, type=Path)
    parser.add_argument("--rust", required=True, type=Path)
    args = parser.parse_args()
    work = ROOT / "target/compat/cases"
    work.mkdir(parents=True, exist_ok=True)
    cases = []
    for folder in sorted((ROOT / "Tests").glob("*/TestData")):
        for fixture in sorted(folder.glob("*.asm")):
            cases.append((fixture, (), folder.parent.name + "_" + fixture.stem))
    for fixture in sorted((ROOT / "Tests/fixtures").glob("*.asm")):
        cases.append((fixture, (), fixture.stem))
        if fixture.name == "instructions.asm":
            cases.append((fixture, ("--raw", "--autozp", "--srec"), fixture.stem + "_options"))
            cases.append((fixture, ("--srec",), fixture.stem + "_srec"))
        if fixture.name == "listing.asm":
            for flags in [("--list0",), ("--list1",), ("--list3",), ("--mlist",)]:
                cases.append((fixture, flags, fixture.stem + "_" + flags[0][2:]))
    generated = {
        "extensions": "  lda low_byte #$1234\n  lda high_byte #$1234\n  lda low_byte $1234\n  lda high_byte $1234\n  lda high_byte <$34\n  lda $1234,x++\n  lda [$34],y++\n  lda [$34].2\n",
        "signed": "  .dw (-8 >> 1), (-8 / 2), (-8 % 3)\n  .db (-1 < 0), (-1 >= 0), (1 <> 2)\n",
        "symbol_functions": "  .bank 2\n  .org $a000\nData: .db 1,2,3\n  .db BANK(Data),PAGE(Data),SIZEOF(Data),DEFINED(missing)\n",
        "constant_scope": "Global:\nVALUE = 3\n.local:\n  jmp .local\n  .db VALUE\n",
        "duplicate_same": "SAME = 1\nSAME = 1\n  .db SAME\n",
        "forward_constant": "A = B\nB = 1\n  .db A\n",
        "conditional_forward": "  .ifndef future\n  .db 1\n  .endif\nfuture:\n  nop\n",
        "undefined_org": "  .org future\nfuture:\n  nop\n",
        "invalid_bank_function": "VALUE = 1\n  .db BANK(VALUE)\n",
        "invalid_size_function": "Label:\n  .db SIZEOF(Label)\n",
        "invalid_vram_function": "Label:\n  .dw VRAM(Label)\n",
        "invalid_pal_function": "Label:\n  .db PAL(Label)\n",
        "macro_types": "types .macro\n  .db \\?1,\\?2,\\?3,\\?4,\\?5,\\?6,\\?7,\\?8,\\?9,\\#\n  .endm\nLabel:\n  types A,#1,$1234,[$12],\"string\",Label\n",
        "macro_unique": "emit .macro\n.local\\@:\n  .db 1\n  .endm\nGlobal:\n  emit\n  emit\n",
        "macro_indexed": "emit .macro\n  lda \\1\n  .endm\n  emit $1234,x\n",
        "autozp_forward": "  .autozp 1\n  lda Variable\n  .zp\nVariable: .ds 1\n",
        "label_org": "Start: .org $8000\n  jmp Start\n",
        "label_data_size": "Data:\n  .db 1,2\n  .db 3\n  .db SIZEOF(Data)\n",
        "bank_resume": "  .bank 0\n  .org $8000\n  .db 1\n  .bank 1\n  .db 2\n  .bank 0\n  .db 3\n",
        "string_escapes": '  .db "A;B", "C\\nD", "\\\""\n',
        "listing_controls": "  .list\n  .bank 0\n  .org $8000\nVALUE = 3\n  .if VALUE\n  .db 1\n  .else\n  .db 2\n  .endif\n  .mlist\n  nop\n  .nomlist\n  .nolist\n",
        "listing_macro": "  .list\nemit .macro\n  lda #\\1\n  .endm\n  .bank 0\n  .org $8000\nStart:\n  emit 1\n  .mlist\n  emit 2\n  rts\n",
        "listing_include": '  .list\n  .bank 0\n  .org $8000\n  .include "helper.asm"\n  rts\n',
        "include_extension": '  .include "helper"\n',
        "bank_name_conflict": '  .bank 1,"one"\n  .bank 1,"two"\n',
        "invalid_macro_name": '.local .macro\n  nop\n  .endm\n',
        "incbin_following_boundary": '  .bank 3\n  .org $ffff\n  .incbin "two.bin"\nAfter:\n  .db 3\n',
        "compact_syntax": 'VALUE=$12\n  .org $8000\nStart:lda #VALUE\n  jmp Start\n',
        "invalid_indented_opcode": '  .org $8000\nStart:\n  nonsense\n',
        "conditional_forward_empty": '  .ifndef future\n  .endif\nfuture:\n  nop\n',
        "bss_high_water": '  .bss\n  .org $400\n  .ds 16\n  .org $200\n  .code\n  .dw _bss_end\n',
        "unterminated_macro_include": '  .include "macro-start.asm"\n  .endm\n  .org $8000\n  emit 42\n',
    }
    (work / "helper.asm").write_text('Included: .db 42\n', encoding="utf-8")
    (work / "two.bin").write_bytes(bytes([1,2]))
    (work / "macro-start.asm").write_text('emit .macro\n  .db \\1\n', encoding="utf-8")
    operands = {"imm":"#$12", "zp":"<$34", "zpx":"<$34,x", "zpy":"<$34,y",
                "zi":"[$34]", "zix":"[$34,x]", "ziy":"[$34],y",
                "abs":"$1234", "ax":"$1234,x", "ay":"$1234,y", "acc":"a",
                "ind":"[$1234]", "ix":"[$1234,x]"}
    modes = {"ADC AND CMP EOR LDA ORA SBC": "imm zp zpx zi zix ziy abs ax ay",
             "ASL LSR ROL ROR": "acc zp zpx abs ax", "BIT": "imm zp zpx abs ax",
             "CPX CPY": "imm zp abs", "DEC INC": "acc zp zpx abs ax",
             "JMP": "abs ind ix", "JSR": "abs", "LDX": "imm zp zpy abs ay",
             "LDY": "imm zp zpx abs ax", "STA": "zp zpx zi zix ziy abs ax ay",
             "STX": "zp zpy abs", "STY": "zp zpx abs"}
    generated["all_addressing_modes"] = "".join(f"  {name} {operands[mode]}\n"
        for names, forms in modes.items() for name in names.split() for mode in forms.split())
    for name, text in generated.items():
        fixture = work / (name + ".asm")
        fixture.write_text(text, encoding="utf-8")
        cases.append((fixture, (), name))
    sjis = work / "sjis.asm"
    sjis.write_bytes("  ; 日本語のコメント\n  .bank 0\n  .org $8000\n  lda #$12\n".encode("cp932"))
    cases.append((sjis, ("--sjis",), "sjis"))
    failed = []
    for fixture, flags, name in cases:
        try:
            compare(args.oracle.resolve(), args.rust.resolve(), fixture.resolve(), work / name, flags)
            print(f"PASS {name}")
        except (AssertionError, subprocess.SubprocessError, ValueError) as error:
            failed.append(name)
            print(f"FAIL {name}: {error}")
    print(f"{len(cases)-len(failed)}/{len(cases)} compatibility cases passed")
    return bool(failed)


if __name__ == "__main__":
    sys.exit(main())

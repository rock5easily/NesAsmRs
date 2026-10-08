"""Differential checks against the unmodified C# core (Windows only).

Usage:
  python tools/compat/compare.py --oracle path.exe --rust target/debug/nesasm.exe
  python tools/compat/compare.py ... --write-golden tools/compat/golden.json

Each case runs the oracle and Rust in separate processes with identical input,
options and working directory. Successful cases compare the ROM, map, header,
symbols (both directions), regions, listing and S-record, and the files Rust
writes (.nes/.s28/.lst bytes, including line endings and encoding). Failing
cases compare the error count and the line numbers of the errors.

Rust intentionally differs from C# in the cases listed in KNOWN_DIFFERENCES
(see docs/rust.md); they are reported as KNOWN and covered by Rust tests.

--write-golden records the C# results of the agreeing cases so that
`cargo test` (crates/nesasm-core/tests/golden.rs) checks them on every OS.
All generated inputs and outputs stay under target/compat/cases.
"""
import argparse
import difflib
import json
import re
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]

RUST_FLAGS = {"--sjis": ["-e", "SJIS"], "--raw": ["-raw"], "--autozp": ["-autozp"],
              "--srec": ["-srec"], "--list3": ["-l3"], "--list1": ["-l1"],
              "--list0": ["-l0"], "--mlist": ["-m"]}

# Cases where Rust deliberately does not reproduce the C# behavior.
KNOWN_DIFFERENCES = {
    # C# defects that Rust does not reproduce.
    "defchr_rows": "C# reads the eight DEFCHR values as one little-endian byte buffer",
    "macro_arg_count": "C# \\# returns 9 when the ninth argument is empty",
    "escaped_quote_later_argument": "C# rejects or crashes on \\\" in a string after the first argument",
    "bank_name_conflict": "C# throws NullReferenceException for a named BANK",
    "db_non_ascii": "C# keeps only the low byte of non-ASCII characters in DB strings",
    # Rust extensions (docs/rust.md).
    "instruction_extensions": "lda.l / lda.h",
    "immediate_low_high": "unary < and > in immediate operands",
}

# Symbols whose value deliberately differs; the rest of the case is compared.
KNOWN_SYMBOL_DIFFERENCES = {
    "AdditionalDirectiveTest_catbank_sample": {
        "_nb_bank": "C# does not count a bank reached through CATBANK",
    },
}

AUX_FILES = {
    "helper.asm": b"Included: .db 42\n",
    "two.bin": bytes([1, 2]),
    "macro-start.asm": b"emit .macro\n  .db \\1\n",
}

GENERATED = {
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
    "macro_types": "types .macro\n  .db \\?1,\\?2,\\?3,\\?4,\\?5,\\?6,\\?7,\\?8,\\?9\n  .endm\nLabel:\n  types A,#1,$1234,[$12],\"string\",Label\n",
    "macro_unique": "emit .macro\n.local\\@:\n  .db 1\n  .endm\nGlobal:\n  emit\n  emit\n",
    "macro_indexed": "emit .macro\n  lda \\1\n  .endm\n  emit $1234,x\n",
    "autozp_forward": "  .autozp 1\n  lda Variable\n  .zp\nVariable: .ds 1\n",
    "label_org": "Start: .org $8000\n  jmp Start\n",
    "label_data_size": "Data:\n  .db 1,2\n  .db 3\n  .db SIZEOF(Data)\n",
    "bank_resume": "  .bank 0\n  .org $8000\n  .db 1\n  .bank 1\n  .db 2\n  .bank 0\n  .db 3\n",
    "string_escapes": '  .db "a\\"b", 1\n',
    "escaped_quote": '  .db "\\""\n',
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
    # Compatibility items fixed after the review (code_review.md section 3).
    "page_directive": "  .bank 0\n  .org $c000\n  .page 7\nlab:\n  nop\n  .page 4\n  .dw lab, *\n",
    "numeric_literals": "  .db 0x1F, 0X0a, %1100_0011\n",
    "column_one_labels": "nop\n\trts\n",
    "column_one_directive": ".bank 0\n  nop\n",
    "column_one_macro": "mymac .macro\n  nop\n  .endm\nmymac\n",
    "multiple_errors": "  .bank 0\n  .org $8000\n  bne far\n  lda #$1234\n  lda missing\n  nop\n  .ds 200\nfar:\n  rts\n",
    "listing_tabs": "\t.list\n\t.bank 0\n\t.org $8000\nStart:\tlda\t#1\t; comment\n\t.db\t1,2\n",
    "listing_data": '  .list\n  .rsset $300\nv1 .rs 2\n  .zp\nzv: .ds 1\n  .code\n  .bank 0\n  .org $c000\n  .incbin "two.bin"\n  .ds 10\n  nop\n',
    "instruction_extensions": "  lda.h $1234\n  lda.l #$1234\n  lda.h #$1234\n",
    "keyword_operators": "  .bank 2\n  .org $a000\nfoo:\n  .db HIGH foo, LOW foo, HIGH foo+1, BANK foo, PAGE foo\nK = $1234\n  .db PAGE(K)\n",
    "paren_expressions": "  jmp ($1234)\n  lda ($10+1)*2,x\n",
    "paren_autozp": "  .autozp 1\nfoo = $10\n  lda (foo)\n  lda (foo,x)\n  lda (foo),y\nbar = $1000\n  lda (bar),y\n",
    "immediate_low_high": "zpv = $12\n  lda #>zpv\n  lda #<$1234\n",
    "indirect_tag_space": "tg = 3\n  lda [$10].tg\n  lda [$10], y\n",
    "if_label": "foo: .if 1\n  nop\n  .endif\n  .dw foo\n",
    "proc_constants": "  .bank 0\n  .org $8000\n  call bar\n  rts\n  .proc foo\n  nop\n  rts\n  .endp\n  .proc bar\nCONST = 5\n  lda #CONST\n  rts\n  .endp\n",
    "opt_listing_only": "  .opt l+\n  nop\n",
    # Known differences (see KNOWN_DIFFERENCES).
    "defchr_rows": "  .defchr $01230123,$12301230,$23012301,$30123012,0,0,0,0\n",
    "macro_arg_count": "count .macro\n  .db \\#\n  .endm\n  count 1,2\n",
    "escaped_quote_later_argument": '  .db "A;B", "a\\"b"\n',
    "string_newline_escape": '  .db "C\\nD"\n',
    "db_non_ascii": '  .db "aあé", 1\n',
}

OPERANDS = {"imm": "#$12", "zp": "<$34", "zpx": "<$34,x", "zpy": "<$34,y",
            "zi": "[$34]", "zix": "[$34,x]", "ziy": "[$34],y",
            "abs": "$1234", "ax": "$1234,x", "ay": "$1234,y", "acc": "a",
            "ind": "[$1234]", "ix": "[$1234,x]"}
MODES = {"ADC AND CMP EOR LDA ORA SBC": "imm zp zpx zi zix ziy abs ax ay",
         "ASL LSR ROL ROR": "acc zp zpx abs ax", "BIT": "imm zp zpx abs ax",
         "CPX CPY": "imm zp abs", "DEC INC": "acc zp zpx abs ax",
         "JMP": "abs ind ix", "JSR": "abs", "LDX": "imm zp zpy abs ay",
         "LDY": "imm zp zpx abs ax", "STA": "zp zpx zi zix ziy abs ax ay",
         "STX": "zp zpy abs", "STY": "zp zpx abs"}
GENERATED["all_addressing_modes"] = "".join(
    f"  {name} {OPERANDS[mode]}\n"
    for names, forms in MODES.items() for name in names.split() for mode in forms.split())


def fnv1a(data):
    """64-bit FNV-1a, also implemented by the Rust golden test."""
    h = 0xcbf29ce484222325
    for b in data:
        h = ((h ^ b) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


def csharp_errors(log):
    """Line numbers of the errors in the C# log, and the reported count."""
    lines = log.replace("\r\n", "\n").split("\n")
    errors = []
    for line, message in zip(lines, lines[1:]):
        match = re.match(r"^\s{0,6}(\d+)\s", line)
        if match and message.startswith("       ") and not message.strip().lower().startswith("warning"):
            errors.append(int(match.group(1)))
    count = re.search(r"# (\d+) error\(s\)", log)
    return errors, int(count.group(1)) if count else len(errors)


def normalize(text, fixture):
    return (text or "").replace("\r\n", "\n").replace(str(fixture), "<input>")


def run_rust(rust, fixture, flags, extra=()):
    rust_flags = [f for flag in flags for f in RUST_FLAGS[flag]]
    return subprocess.run([str(rust), "--json", *extra, *rust_flags, str(fixture)],
                          cwd=fixture.parent, capture_output=True, text=True,
                          encoding="utf-8", timeout=30)


def compare(oracle, rust, fixture, output, flags=(), ignored_symbols=()):
    """Compares one case; returns the golden record of the C# result."""
    output.mkdir(parents=True, exist_ok=True)
    old_file = output / "result.json"
    old_file.unlink(missing_ok=True)
    subprocess.run([str(oracle), str(fixture), str(output), *flags], check=True, timeout=30)
    if not old_file.exists():
        raise AssertionError("C# oracle exited without reporting a result")
    old = json.loads(old_file.read_text(encoding="utf-8-sig"))
    proc = run_rust(rust, fixture, flags, ["--check"])
    if not proc.stdout:
        raise AssertionError(f"Rust did not report a result: {proc.stderr}")
    new = json.loads(proc.stdout)
    (output / "rust.json").write_text(json.dumps(new), encoding="utf-8")
    if old["success"] != new["success"]:
        raise AssertionError(f"success differs\nC#: {old['log']}\nRust: {new['diagnostics']}")
    rust_errors = sorted(d["location"]["line"] for d in new["diagnostics"] if d["severity"] == "error")
    expected_status = 0 if new["success"] else max(1, min(len(rust_errors), 255))
    if proc.returncode != expected_status:
        raise AssertionError(f"Rust exit status {proc.returncode}, expected {expected_status}")
    if not old["success"]:
        # C# may report several errors for one line; compare the lines.
        errors = sorted(set(csharp_errors(old["log"])[0]))
        if errors != sorted(set(rust_errors)):
            raise AssertionError(f"error lines {errors} != {sorted(set(rust_errors))}\n"
                                 f"C#: {old['log']}\nRust: {new['diagnostics']}")
        return {"success": False, "errors": errors}
    for field in ("binary", "map", "header"):
        if old[field] != new[field]:
            a, b = old[field], new[field]
            index = next((i for i, (x, y) in enumerate(zip(a, b)) if x != y), min(len(a), len(b)))
            raise AssertionError(f"{field} differs at {index:#x}; lengths {len(a)}/{len(b)}")
    if set(old["symbols"]) != set(new["symbols"]):
        raise AssertionError(f"symbol sets differ: only C# {sorted(set(old['symbols']) - set(new['symbols']))}, "
                             f"only Rust {sorted(set(new['symbols']) - set(old['symbols']))}")
    for name, symbol in old["symbols"].items():
        if name in ignored_symbols:
            continue
        for field in ("value", "bank", "size"):
            if symbol[field] != new["symbols"][name][field]:
                raise AssertionError(f"Symbol {name} {field}: {symbol[field]} != {new['symbols'][name][field]}")
    for name, region in old["regions"].items():
        if new["regions"].get(name, {}).get("size") != region["size"]:
            raise AssertionError(f"Region {name} differs")
    if normalize(old["srec"], fixture) != normalize(new["srec"], fixture):
        raise AssertionError("S-record differs")
    if normalize(old["listing"], fixture) != normalize(new["listing"], fixture):
        diff = "\n".join(difflib.unified_diff(normalize(old["listing"], fixture).splitlines(),
                                              normalize(new["listing"], fixture).splitlines()))
        raise AssertionError(f"Listing differs:\n{diff[:2500]}")
    compare_files(rust, fixture, output, flags, old)
    return {
        "success": True,
        "binary": fnv1a(old["binary"]),
        "map": fnv1a(old["map"]),
        "header": old["header"],
        "symbols": {name: [s["value"], s["bank"], s["size"]]
                    for name, s in old["symbols"].items() if name not in ignored_symbols},
        "ignored_symbols": sorted(ignored_symbols),
        "regions": {name: r["size"] for name, r in old["regions"].items()},
        "listing": normalize(old["listing"], fixture) if old["listing"] else None,
        "srec": normalize(old["srec"], fixture) if old["srec"] else None,
    }


def compare_files(rust, fixture, output, flags, old):
    """Compares the files Rust writes with the C# files and listing text."""
    rom = output / "rust.nes"
    for path in (rom, rom.with_suffix(".lst"), rom.with_suffix(".s28")):
        path.unlink(missing_ok=True)
    proc = run_rust(rust, fixture, flags, ["--output", str(rom)])
    if proc.returncode != 0:
        raise AssertionError(f"Rust failed to write artifacts: {proc.stdout[-500:]}")
    encoding = "cp932" if "--sjis" in flags else "utf-8"
    if "--srec" in flags:
        if (output / "rom.s28").read_bytes() != rom.with_suffix(".s28").read_bytes():
            raise AssertionError(".s28 file differs")
    elif (output / "rom.nes").read_bytes() != rom.read_bytes():
        raise AssertionError(".nes file differs")
    listing = rom.with_suffix(".lst")
    if old["listing"]:
        written = listing.read_bytes().decode(encoding)
        if written.replace(str(fixture), "<input>") != old["listing"].replace(str(fixture), "<input>"):
            raise AssertionError(".lst file differs (bytes, line endings or encoding)")
    elif listing.exists():
        raise AssertionError("Rust wrote a listing that C# does not produce")


def cases(work):
    """(fixture, flags, name, golden source) for every case."""
    result = []
    for folder in sorted((ROOT / "Tests").glob("*/TestData")):
        for fixture in sorted(folder.glob("*.asm")):
            result.append((fixture, (), folder.parent.name + "_" + fixture.stem,
                           {"file": fixture.relative_to(ROOT).as_posix()}))
    for fixture in sorted((ROOT / "Tests/fixtures").glob("*.asm")):
        source = {"file": fixture.relative_to(ROOT).as_posix()}
        result.append((fixture, (), fixture.stem, source))
        if fixture.name == "instructions.asm":
            result.append((fixture, ("--raw", "--autozp", "--srec"), fixture.stem + "_options", source))
            result.append((fixture, ("--srec",), fixture.stem + "_srec", source))
        if fixture.name == "listing.asm":
            for flags in [("--list0",), ("--list1",), ("--list3",), ("--mlist",)]:
                result.append((fixture, flags, fixture.stem + "_" + flags[0][2:], source))
    for name, data in AUX_FILES.items():
        (work / name).write_bytes(data)
    for name, text in GENERATED.items():
        fixture = work / (name + ".asm")
        fixture.write_text(text, encoding="utf-8", newline="\n")
        result.append((fixture, (), name, {"text": text}))
    sjis_text = "  ; 日本語のコメント\n  .bank 0\n  .org $8000\n  lda #$12\n"
    sjis = work / "sjis.asm"
    sjis.write_bytes(sjis_text.encode("cp932"))
    result.append((sjis, ("--sjis",), "sjis", {"sjis_text": sjis_text}))
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--oracle", required=True, type=Path)
    parser.add_argument("--rust", required=True, type=Path)
    parser.add_argument("--write-golden", type=Path)
    args = parser.parse_args()
    work = ROOT / "target/compat/cases"
    work.mkdir(parents=True, exist_ok=True)
    failed, golden = [], []
    all_cases = cases(work)
    for fixture, flags, name, source in all_cases:
        try:
            ignored = KNOWN_SYMBOL_DIFFERENCES.get(name, {})
            record = compare(args.oracle.resolve(), args.rust.resolve(), fixture.resolve(),
                             work / name, flags, ignored)
            for symbol, reason in ignored.items():
                print(f"KNOWN {name} symbol {symbol}: {reason}")
            if name in KNOWN_DIFFERENCES:
                print(f"PASS {name} (listed as a known difference; remove it from KNOWN_DIFFERENCES)")
            else:
                print(f"PASS {name}")
                golden.append({"name": name, "source": source, "flags": list(flags), "expected": record})
        except (AssertionError, subprocess.SubprocessError, ValueError) as error:
            if name in KNOWN_DIFFERENCES:
                print(f"KNOWN {name}: {KNOWN_DIFFERENCES[name]}")
            else:
                failed.append(name)
                print(f"FAIL {name}: {error}")
    print(f"{len(all_cases) - len(failed)}/{len(all_cases)} compatibility cases passed "
          f"({sum(1 for c in all_cases if c[2] in KNOWN_DIFFERENCES)} known differences)")
    if args.write_golden and not failed:
        document = {"aux_files": {k: list(v) for k, v in AUX_FILES.items()}, "cases": golden}
        args.write_golden.write_text(json.dumps(document, indent=1, ensure_ascii=False) + "\n",
                                     encoding="utf-8", newline="\n")
        print(f"wrote {len(golden)} golden cases to {args.write_golden}")
    return bool(failed)


if __name__ == "__main__":
    sys.exit(main())

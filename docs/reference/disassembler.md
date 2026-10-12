# Disassembler

The MCP tools `disassemble` and `export_disassembly` turn a ROM into NESASM
source that reassembles to the same bytes. An annotation file records what is
known about the ROM; edit it and disassemble again until the source reads well.

## Workflow

1. Create an annotation file in the project root, for example
   `game.disasm.json` containing `{"rom": "old/game.nes"}`.
2. Call `disassemble` with `"annotations": "game.disasm.json"`. Read the
   summary, the issues and `verification`, and look at ranges with `view`,
   `xrefs`, `routines` and `ram`.
3. Compare with the author's notes and specifications. Record names, data
   ranges and comments in the annotation file; settle the issues.
4. Repeat 2-3. Then call `export_disassembly`; it writes the source only if it
   reassembles to the same ROM. Check it with `check` (`options.raw` for
   `.bin` ROMs).

Addresses in annotations and queries are numbers or strings such as `"$C000"`
or `"0xC000"`. Reports show them as `"$C000"`. `bank` may be omitted when only
one PRG bank holds the address.

## ROM hacks

For a ROM hack, give the original ROM as `base`. Only the ranges that differ
from it become source. The main file includes the base ROM once, without its
header (`.bank 0`, `.org`, `.incbin "base.nes", 16, size`), and the source
then writes each changed range over it with `.bank` and `.org`, so the
source reassembles to the hack when the base ROM is present. The header comes
from the hack. A bank without changes has no lines (and, when the source is
split, no bank file). An expanded ROM whose CHR banks moved after new PRG banks
includes the base in one `.incbin` per run of banks that keep their order. No
`.catbank` is needed: `.incbin` continues into the next banks by itself. `disassemble` returns the changed ranges as
`diffs` (the first 20; select with `offset` and `limit`), each with:

- `changed_bytes` and the `code_bytes`, `data_bytes` and `unknown_bytes` of
  the range in the hack;
- `base_fill`: the value the base held throughout the range, when the range
  was free space (new code or data rather than a change);
- `labels`, `routines`, and `referenced_from`: references from outside the
  range, such as the hook (a changed JSR or JMP) into new code;
- the first `bytes` of the hack and `base_bytes` of the base.

Bytes that differ less than 4 bytes apart are one range, and ranges are
widened to whole instructions and table entries. To make more of the ROM
source (a whole routine around a change, a table), add `patches`. Code is
still traced through the whole hack, so labels and routines are known;
labels in unchanged ranges that the source uses are defined as constants
(`L_E00B = $E00B`). Issues are reported only within the changed ranges.
PRG banks are matched with the base's PRG banks in order, and CHR banks with
its CHR banks, so an expanded ROM lines up; banks the base lacks are
disassembled whole. Changed CHR banks are written whole. The base must have
the same format as `rom` (`.nes` or whole-bank `.bin`), without a trainer;
fragments (`load_address`) cannot have a base.

## Annotation file

- `rom`: the ROM file relative to the root: `.nes` (iNES) or a raw `.bin`.
- `load_address`: for a raw fragment, the CPU address of its first byte.
  Required unless the file is a whole number of 8 KiB banks.
- `base`: for a ROM hack, the original ROM relative to the root (see above).
- `patches`: `[{"start", "end" | "length", "bank", "comment"}]`, with `base`:
  ranges to disassemble besides the changed bytes. The comment is placed above
  the range.
- `banks`: `[{"bank": 2, "org": "$8000"}]`, the CPU window of PRG banks
  (multiples of $2000 up to $E000). By default the last two PRG banks are at
  $C000/$E000 and the others alternate $8000/$A000; a single bank is at $E000.
  CHR banks follow the PRG banks.
- `entry_points`: `[{"address", "bank", "name", "comment", "no_return", "inline_bytes"}]`.
  Code is traced from the interrupt vectors and these addresses. `no_return`:
  a JSR to the routine does not return. `inline_bytes`: bytes of inline
  arguments after each JSR to it, emitted as data (or as the regions that
  describe them).
- `labels`: `[{"address", "bank", "name", "comment"}]`, names of ROM
  addresses. The comment is placed above the label.
- `symbols`: `[{"name", "value", "size", "comment"}]`, names of RAM, I/O and
  other addresses outside the ROM, emitted as `name = $xxxx`. Operands inside
  `size` bytes are written `name+n`.
- `regions`: `[{"start", "end" | "length", "bank", "type", "comment", "columns"}]`.
  Types: `code` (decode every instruction in the range), `bytes` (`.db`,
  default), `words` (`.dw`), `pointers` (`.dw label`), `low_bytes` /
  `high_bytes` (split address tables, `.db LOW(label)` / `.db HIGH(label)`;
  `pair` on `low_bytes` is the start of the `high_bytes` table of the same
  length), `text` (ASCII as `.db "..."`). For tables, `code_targets: true`
  makes the targets code entry points (jump tables) and `adjust` is the stored
  value minus the target (`-1` for RTS jump tables, written `label-1`).
- `comments`: `[{"address", "bank", "text", "position"}]`; `position` is
  `line` (end of the line, default) or `above` (lines of their own).
- `operands`: `[{"address", "bank", "expr"}]` replaces the operand of the
  instruction (`#LOW(Table)`, `<ptr+1`, `[ptr],y`) or a data value at the
  address. The round-trip check catches expressions that change the bytes.
- `references`: `[{"address", "bank", "target_bank"}]` selects the bank of a
  reference (instruction or table entry at `address`) to an address several
  banks hold.
- `options`: `uppercase` (mnemonics, default false), `data_per_line`
  (default 8), `address_comments` (append `; $C000`), `auto_ram_labels`
  (`zp_10`, `ram_0300`), `nes_registers` (PPUCTRL ... JOY2, default true),
  `chr` (`incbin`, default, or `db`), `split_banks` (one file per bank; by
  default only when the source would exceed 512 KiB).

Names use letters, digits and `_`, do not start with a digit and are at most
64 characters. `A`, `X`, `Y`, expression function names and assembler symbols
are reserved.

## Generated source

- Automatic labels: `sub_C123` (JSR targets), `L_C123` (jumps and branches),
  `D_C123` (data read by code or named by tables), and `NMI`, `RESET`, `IRQ`
  for the vectors. The vectors are always traced; those no region describes
  are written as a pointer table. When several banks hold the address, the bank is included
  (`L_03_8123`). A target inside an instruction, or in the second byte of a
  word of a `words`/`pointers` region, is written `label+n`; a named one is
  defined as `name = label+n`, so instructions and words are never split.
  `operands` cannot replace the second byte of a word.
- Zero page operands use `<`, absolute ones keep four digits (`lda $0012`),
  so no AUTOZP is needed. Branches that wrap around $FFFF use `*+n`.
- Bytes no code reaches and no region describes are `.db`; runs of 16 or more
  equal bytes are `.ds n` or `.ds n, fill`.
- CHR banks are `.incbin "<stem>.chr", offset, size` (written next to the
  source by `export_disassembly`) or `.db` lines.
- Instructions and directives are indented by one tab; labels, constants and
  comments start the line.
- Paths in `.include` and `.incbin` are relative to the project root.
- Bank files are `<stem>_bankXX.asm`, with the bank number in two uppercase
  hexadecimal digits (`_bank0A.asm` for bank 10, `_bank3F.asm` for bank 63),
  included from the main file. Older versions used decimal (`_bank10.asm` for
  bank 10), so an old name may now name another bank.
- The first line of each file is `; Generated by nesasm disassembler; checksum ...`.
  `export_disassembly` replaces an existing file only if it is such a file
  and its checksum still matches (it was not edited), and only by a file of
  the same kind: the main file by the main file, and a bank file by the file
  of the same bank (told by its `.bank` line, not its name). The exception
  is a bank file that the replaced main file includes: the whole set is
  generated again, so it is replaced whatever its bank. Unedited bank files
  the replaced main file includes that the new source does not (old decimal
  names, banks a ROM hack no longer changes) are removed and reported as
  `removed`; edited ones are kept. Without the old main file, an old name that
  now names another bank is an error, and nothing is written. The main file
  also records the checksum of the `.chr` file, which is replaced only together
  with an unedited main file and only if it was not changed either. It writes
  only `.asm` files and the `.chr` file (also after following links), outside
  hidden directories and never over the ROM. If one of the files cannot be
  replaced (for example, another program has it open), none of them is
  changed and no file is removed.
- Paths written into `.include` and `.incbin` (the output and `base`) cannot
  contain quotes or control characters, nor (outside Windows) backslashes.
  Source files that are not valid UTF-8 are never replaced.

## Issues

`header` (fields NESASM cannot reproduce: NES 2.0 bytes, trainer, size
mismatch), `invalid_opcode` (flow reaches a byte that is not an NMOS 6502
instruction: wrong flow or data), `overlap` (code enters the middle of an
instruction, as in BIT skip tricks), `code_data_conflict` (code reaches a data
region), `indirect_jump` (`JMP [ptr]`: describe the targets with a pointer
table or entry points), `unresolved_bank` (add `references`), `outside_rom`
(code in RAM or SRAM is called), `flow_leaves_rom` (code runs off a bank),
`mapper_write` (a write to $8000-$FFFF, usually bank switching), and
`unknown_range` (bytes not described; padding runs are not reported).
65C02-only encodings are treated as data.

## Limits

Annotation values must stay within the ROM: ranges within one bank,
`inline_bytes` at most 8192, symbols ending at or below $FFFF. Table values
whose adjusted target would wrap around $FFFF are written as numbers.

ROMs of up to 128 banks of 8 KiB; over MCP, ROM and annotation files of up
to 8 MiB each (also the base ROM). A disassembly is subject to the server's time limit.

## Queries

`disassemble` returns the first 20 issues; `issues` selects a `kind`,
`offset` and `limit`. `view` returns the source lines of `from`-`to` (default
256 bytes) with their bytes. `xrefs` lists the references to each label,
symbol or address. `routines` lists code reached from each entry without
following calls (also into the next bank), with callers, calls and the RAM
it reads and writes; a routine is followed for up to 16384 instructions and
marked `truncated` beyond. `ram` lists addresses outside the ROM with read
and write counts and every routine that accesses them.

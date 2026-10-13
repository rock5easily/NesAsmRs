# Disassembler

The MCP tools `disassemble` and `export_disassembly` turn a ROM into NESASM
source that reassembles to the same bytes. An annotation file records what is
known about the ROM; change it with `annotate` and disassemble again until the
source reads well.

## Workflow

The server does not invoke an LLM. A person or an LLM client interprets the
reports and source evidence, then supplies annotations for the next analysis.
`annotate` applies them to the annotation file (see
[Changing annotations](#changing-annotations)); the file can also be edited by
hand, or the config passed as `inline` instead of `annotations`. Paths in the
config, including `rom` and `base`, are relative to the server root, even when
the annotation file is in a subdirectory.
For a detailed Japanese guide, see [MCP and LLM workflow](../disassembly-workflow.md).

1. Create an annotation file in the project root, for example
   `game.disasm.json` containing `{"rom": "old/game.nes"}` (or with
   `annotate` and `create`).
2. Call `disassemble` with `"annotations": "game.disasm.json"`. Read the
   summary, the issues and `verification`, and look at ranges with `view`,
   `xrefs`, `routines` and `ram`.
3. Compare with the author's notes and specifications. Record names, data
   ranges and comments with `annotate`, with the evidence for them; settle
   the issues (their `suggestions` are operations for `annotate`).
4. Repeat 2-3. Then call `export_disassembly`; it writes the source only if it
   reassembles to the same ROM. Check it with `check` (`options.raw` for
   `.bin` ROMs).

Keep the initial annotations and report as a baseline. Each iteration should
record the evidence for new entries, bank references and bounded data tables,
then check newly reached code against known data and conflicts. Untraced bytes
emitted as `.db` are unknown, not necessarily confirmed data. Do not force
whole unknown ranges to `code` or infer quality from code-byte counts alone.
Byte-exact reassembly checks reproducibility, not the correctness of the
code/data classification. Export to a fresh path, assemble to a different
path from either input ROM, and compare the whole files, including the header.

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
includes the base in one `.incbin` per run of banks that keep their order. The base
needs no `.catbank`: `.incbin` continues into the next banks by itself. `disassemble` returns the changed ranges as
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
disassembled whole. The base ROM's `.incbin` covers its PRG banks only. The
whole CHR ROM of the hack, changed or not, is extracted to `<stem>.chr` in its
original order (edit it with a CHR tool) and included once at the end of the
main file: `.bank` of the first CHR bank (the number of 8 KiB PRG banks),
`.org $0000`, then `.incbin "<stem>.chr"`. A split source has no bank files of
CHR banks, and a ROM with CHR RAM has no CHR file. `diffs` still list the
changed CHR banks with their `changed_bytes`; the extraction does not count as
a change. `chr: "db"` does not apply with `base` and is an error. The base must have
the same format as `rom` (`.nes` or whole-bank `.bin`), without a trainer;
fragments (`load_address`) cannot have a base.

## How code is traced

Code is traced from the vectors, `entry_points`, `code` regions and jump
tables, following JSR, JMP and branches. The disassembler never decodes
unreached bytes as code: what is not traced stays `.db` (`unknown_range`).
Along each path it keeps the values it knows, so that it can follow code that
only runs after some setup. Where it cannot be sure, a value is unknown and
nothing is inferred from it:

- Registers, RAM ($0000-$07FF, $6000-$7FFF) and pushed values that are
  constants: loaded immediately, from RAM written on the path, or from a ROM
  byte; moved, incremented, added to. A write whose address is not known
  (indexed by an unknown register, or through a pointer) forgets the RAM it
  may reach, and one through a pointer that is not known also the banks.
  Reads indexed by a known X/Y, and reads through a known zero-page pointer
  with a known index, also keep the value. Zero-page indexing and pointer
  bytes wrap at $FF; absolute and indirect indexed addresses wrap at $FFFF.
  Unknown indices or unresolved ROM banks do not select an entry.
  Indexed writes wrap around $FFFF.
- The PRG banks at $8000 and $A000. In a switched bank ($8000/$A000 window of
  the layout), code runs with its own bank switched in; code that writes
  another bank, or one that is not known, into its own window is not
  followed further, nor is code after a call to a routine that writes the
  bank of that window to the mapper itself (or through the routines it
  calls) unless the bank it leaves is known. A call whose effects are not
  known, and a write through a pointer that is not known, are taken to
  leave the bank the code runs in (a convention: code does not switch its
  own bank out that way). On mapper 4
  (MMC3), writes of known values to the bank select ($8000) and bank data
  ($8001) registers switch R6 ($8000) and R7 ($A000) in PRG mode 0; a bank
  select for PRG mode 1 or of an unknown value, and any other write to the
  bank data register, make the bank at $8000 (or both) unknown. References
  into $8000-$BFFF resolve to the bank switched in there; if the layout places
  that bank at another window, the reference is `unresolved_bank`, naming the
  `banks` entry to add. A call into a switched bank runs with the caller's
  banks.
- What a called routine does: each of its paths is followed with symbolic
  values (up to 4096 instructions in all, calls nested 6 deep; paths that
  join or loop back are walked again knowing only what they share, until
  they know nothing new), giving the returned A/X/Y, banks and RAM in terms of the caller's registers
  and RAM. A routine that switches the bank in A, or the banks a game keeps in
  RAM, is recognised from its code; the caller then knows the banks after the
  call. A routine with too many paths keeps the banks only if no code it
  reaches writes to $8000-$FFFF or jumps where the scan cannot follow. A path
  through code that cannot be followed (an unresolved indirect jump, a routine that is
  not known) makes the banks and returned registers after the call unknown.
  A known indirect tail JMP is followed in the summary too. Returned
  registers are kept only when every return agrees; an unknown exit also
  makes them unknown. If the routine would
  switch in banks held in RAM were that code to change nothing,
  `bank_switch` names the routine and the `switch_banks` to give it in
  `entry_points` if it always does.
- Stack memory is not used to recover registers across a call: JSR and
  pushes overwrite bytes at unknown positions in $0100-$01FF. A store that
  may reach that page (including its internal RAM mirrors), or a call whose
  RAM effects are unknown, also invalidates saved pushed values. A balanced
  PHA/PLA around code with known effects can still preserve a register.
- Where paths that know different values join, the code is traced again
  knowing only what they share (up to 16 times), so a reference never uses a
  bank that only one of the paths switched in.

From these values it also follows:

- Indirect jumps through a pointer the path knows (`JMP [$xxFF]` reads its
  high byte from $xx00, as the NMOS 6502 does), and the constant addresses
  code stores into the pointer of an indirect JMP elsewhere (thread entries,
  interrupt handlers that chain): two stores in a row into its two bytes, still
  unchanged when the path jumps, calls or branches, unless code also reads
  data through that pointer (`lda [ptr],y`) or the address is in
  $8000-$BFFF (the bank there at the jump is not known). The `indirect_jump`
  issue lists where they were stored.
- A return address pushed before a jump (`pha` of a constant address-1): the
  code there runs when the routine jumped to returns, if a path of it that
  can be followed returns. Before a jump through a pointer that is not known,
  `indirect_jump` names the address to give in `entry_points` if the targets
  return. An RTS after pushing a constant address jumps to it plus one.
- Routines that take their return address. One that jumps through a pointer
  read through it (a jump table after each JSR to it) makes the words after
  each JSR a jump table: an entry at a time, while the two bytes are
  unclassified, before the first target that follows the table, and point to
  an instruction in the banks of the call (up to 128 entries); `jump_table`
  reports each table to confirm. One that reads arguments after the JSR stops
  the trace after the JSR (`return_address`), until `entry_points` gives its
  `inline_bytes`.
- For jumps through values read from ROM tables (`lda table,x` stored into
  the pointer, or pushed before an RTS), `indirect_jump` names the tables and
  the region to add (`pointers`, or `low_bytes` with its `pair`, and
  `adjust: -1` for RTS jumps).

Annotations take precedence: `references` over the banks a path knows,
`entry_points` with `no_return`, `inline_bytes`, `switch_banks` or `keeps_banks` over what a
routine is found to do, and regions over jump tables found after a JSR.
Writes to $8000-$FFFF are mapper registers: their operands are numbers
(`sta $8000`), and the `mapper_write` issue explains the MMC3 registers.

## Mapper 4 fixed PRG banks

MMC3 games usually keep the last two 8 KiB PRG banks at $C000-$DFFF and
$E000-$FFFF. With `options.mapper4_fixed_prg_16k: true`, the analysis treats
these two banks as one range, $C000-$FFFF:

- Instructions may cross $DFFF/$E000. They are decoded, traced and written as
  one instruction, whose source line holds all its bytes. Branches, calls,
  references and routines continue across the boundary, and each of the two
  banks resolves addresses in the other before other banks.
- The source writes `.catbank XX` (XX the second-to-last bank in hexadecimal)
  before the second-to-last bank, so the assembler continues it into the last
  bank. No `.bank` or `.org` is written inside an instruction: where the last
  bank starts inside one, the line `; Bank NN starts at $E000, inside the
  instruction above` marks it; otherwise the last bank starts with `.bank` and
  `.org $E000` as usual. Bank numbers stay those of the 8 KiB banks.
- With `split_banks`, both banks are written to `<stem>_bankXX_YY.asm`
  (`_bank1E_1F.asm` for banks 30 and 31), which the main file includes after
  `.catbank XX`. Without `split_banks`, `.catbank XX` precedes the
  second-to-last bank's `.bank` line.
- In a ROM hack, a change in either part of an instruction across the
  boundary widens the ranges of both banks to the whole instruction, and the
  range of the last bank continues it without `.org`. The file of the two
  banks is written when either of them has changes.
- The ROM must be an iNES ROM of mapper 4 with at least two whole PRG banks;
  otherwise the option is an error. `banks` may only give the two banks $C000
  and $E000. Annotation ranges (`regions`, `patches`) still lie within one
  bank.
- The option only states where the two banks are. The disassembler does not
  model PRG mode 1 (the second-to-last bank at $8000) or mode changes while
  the game runs; writes to the mapper registers remain `mapper_write` issues.
- Without the option (the default), each bank is analysed and written on its
  own, and an instruction across $DFFF/$E000 is an `overlap` issue whose
  bytes are data.

## Changing annotations

`annotate` changes an annotation file by operations, checks the result and
writes it as one change:

```json
{
  "annotations": "game.disasm.json",
  "expected_sha256": "<sha256 of the file as read>",
  "operations": [
    {"op": "upsert", "kind": "references",
     "item": {"bank": 30, "address": "$DEB1", "target_bank": 27},
     "evidence": "A=$1B before the call; the switch routine restores the bank on every return"},
    {"op": "remove", "kind": "labels", "key": {"name": "OldName"}},
    {"op": "set", "kind": "options", "item": {"address_comments": true}}
  ]
}
```

- `upsert` adds `item`, a whole entry with the fields of the annotation file,
  and replaces the entries with the same key. It does not merge fields: the
  result reports the entries it replaced (`applied[].previous`) to change and
  send again. Keys: `entry_points`, `operands`, `references`: bank and
  address; `regions`, `patches`: bank and start; `comments`: bank, address
  and position; `labels`, `symbols`: name; `banks`: bank. An item cannot
  leave a field null (fill in what a suggestion `needs`), and regions and
  patches give `end` or `length`.
- `remove` removes every entry that `key` (`bank`, `address` or `start`,
  `name`, `position`) names, and fails if none does. A comment key without
  `position` names both positions.
- `set` (kind `options`) changes the fields of `options` that `item` gives;
  null returns a field to its default. `rom`, `base` and `load_address` are
  not changed by operations: another ROM is another annotation file.
- Operations apply in order, those on `banks` and `options` first, as the
  bank of the other entries depends on them. An address without `bank` must
  be held by one bank of the resulting layout. The result is checked as a
  whole: duplicate keys (also ones the file already had), overlapping
  regions and the other annotation errors fail, naming the operation where
  they come from one.
- `evidence` is required for entries that override what the analysis finds:
  `references`, `entry_points` with `no_return`, `inline_bytes`,
  `switch_banks` or `keeps_banks`, and `regions` of type `code` or with
  `code_targets`.
- The file is written only if the operations apply, the annotations are
  valid, the source reassembles to the same ROM (`require_match`, default
  true) and the file's SHA-256 is still `expected_sha256` (the `sha256` of
  the previous `annotate`, or `sha256_before`, which every result for an
  existing file gives). With `create` (`rom`, and `base` or `load_address`)
  the file must not exist. `dry_run` writes nothing.
- The file is written in canonical form: `rom`, `load_address`, `base`,
  `options`, then the lists (`banks`, `entry_points`, `labels`, `symbols`,
  `regions`, `comments`, `operands`, `references`, `patches`) with one entry
  per line, ordered by bank and address (symbols by value), each naming its
  bank, addresses as `"$C000"` and fields at their default values left out.
  `reformatted` tells that a file was not in that form before.
- Each write appends a line to `<name>.disasm.log.jsonl` beside the file: the
  time, the operations with their evidence, the SHA-256 before and after, and
  PRG totals (instructions, code, data and unknown bytes, issues) before and
  after. The result gives the same `log_entry`, also for a dry run or
  `inline` annotations (then nothing is written and the changed
  `annotations` are returned).
- The result also gives the summaries before and after, `verification`, and
  the issues resolved, introduced and changed (`issues_changed`: reported
  before and after with another message, with its `previous_message`),
  matched by `id`.

Only files named `*.disasm.json` inside the root and outside hidden
directories are written (also after following links; a link to a file or
directory that does not exist is refused), with their log.

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
- `entry_points`: `[{"address", "bank", "name", "comment", "no_return", "inline_bytes", "switch_banks", "keeps_banks"}]`.
  Code is traced from the interrupt vectors and these addresses. `no_return`:
  a JSR to the routine does not return. `inline_bytes`: bytes of inline
  arguments after each JSR to it, emitted as data (or as the regions that
  describe them). `switch_banks`: `["$F5", "$F6"]`, the RAM addresses holding
  the PRG banks the routine switches in at $8000 and $A000 (`null` for a
  window it leaves); a call to the routine, or code going on into it, then
  switches those banks in and changes no other RAM the caller knows. If the
  value in the RAM is not known at the call, the bank there is not known,
  and code calling it from that window is taken to keep its own bank (the
  convention for calls above). The addresses must be RAM ($0000-$07FF, $6000-$7FFF). `keeps_banks: true`: the
  routine returns with the banks it was called with (such as a thread yield,
  during which other code switches banks); nothing else about it is assumed.
  An entry point gives `switch_banks` or `keeps_banks`, not both.
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
  `bank_table` on `pointers` and `low_bytes` is the start of a table in the
  same bank with a byte per entry: the PRG bank (8 KiB) switched in for that
  entry. A target that bank holds resolves to it; other targets (the fixed
  banks, the table's own bank) resolve as usual. Code targets run with the
  table's bank and the entry's bank switched in.
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
  `chr` (`incbin`, default, or `db`; with `base`, only `incbin`),
  `split_banks` (one file per bank; by default only when the source would
  exceed 512 KiB), `mapper4_fixed_prg_16k`
  (mapper 4: the last two PRG banks as one range at $C000-$FFFF, see above;
  default false).

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
  source by `export_disassembly`) or `.db` lines. A ROM hack includes the whole
  `<stem>.chr` once (see ROM hacks).
- Writes to $8000-$FFFF (mapper registers) keep numeric operands.
- Instructions and directives are indented by one tab; labels, constants and
  comments start the line.
- Paths in `.include` and `.incbin` are relative to the project root.
- Bank files are `<stem>_bankXX.asm`, with the bank number in two uppercase
  hexadecimal digits (`_bank0A.asm` for bank 10, `_bank3F.asm` for bank 63),
  included from the main file; with `mapper4_fixed_prg_16k`, the last two PRG
  banks share `<stem>_bankXX_YY.asm`. A ROM hack includes its CHR ROM in the
  main file. Older versions used decimal (`_bank10.asm` for
  bank 10), so an old name may now name another bank.
- The first line of each file is `; Generated by nesasm disassembler; checksum ...`.
  `export_disassembly` replaces an existing file only if it is such a file
  and its checksum still matches (it was not edited), and only by a file of
  the same kind: the main file by the main file, and a bank file by the file
  of the same bank (told by its `.bank` line, not its name). The exception
  is a bank file that the replaced main file includes: the whole set is
  generated again, so it is replaced whatever its bank. Unedited bank files
  the replaced main file includes that the new source does not (old decimal
  names, banks a ROM hack no longer changes, the CHR bank files of a ROM hack
  from older versions, the files of the two banks that
  `mapper4_fixed_prg_16k` combines) are removed and reported as `removed`;
  edited ones are kept. Without the old main file, an old name that
  now names another bank is an error, and nothing is written. The main file
  also records the checksum of the `.chr` file, which is replaced only together
  with an unedited main file and only if it was not changed either (so the
  CHR file of the changed banks that older versions wrote for a ROM hack is
  replaced by the whole CHR ROM). It writes
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
instruction, as in BIT skip tricks, or an instruction would cross the end of a
bank; see `mapper4_fixed_prg_16k`), `code_data_conflict` (code reaches a data
region), `indirect_jump` (`JMP [ptr]`, or an RTS after pushes, whose targets
are not known: describe them with a pointer table or entry points; the
message names the tables the pointer was read from, or where addresses
stored into it were followed), `jump_table` (a jump table found after a JSR
to a routine that jumps through its return address: confirm its length with
a `pointers` region), `return_address` (a routine takes its return address,
so the code after the JSR is not followed: give its `inline_bytes`),
`bank_switch` (a routine switches in banks held in RAM on the paths that can
be followed, but other paths run code that cannot: give it `switch_banks` if
it always does),
`unresolved_bank` (add `references`, or the `banks` entry the message names
for a bank switched in where the layout does not place it), `outside_rom`
(code in RAM or SRAM is called), `flow_leaves_rom` (code runs off a bank),
`mapper_write` (a write to $8000-$FFFF, usually bank switching; MMC3 writes
are explained), and `unknown_range` (bytes not described; padding runs are
not reported). 65C02-only encodings are treated as data.

Each issue has an `id` that identifies it across analyses of the ROM: its
kind, bank (`-` without one) and address, such as `unresolved_bank:30:$DEB1`;
header issues, which all name $0000, add a topic (`header:-:$0000:trainer`,
also `nes2`, `size` and `partial_bank`). Further issues of the same kind at
the same place, which come from different paths, add `#2`, `#3` in the order
they are reported. The id does not depend on the message, the path that
reached the place or the annotations, so an issue keeps it while other
annotations change; in a ROM hack it is the id of the whole ROM's analysis.

Issues whose settlement the analysis can shape have `suggestions`, each one
alternative: `description`, the `operations` to give `annotate` together,
`needs` (fields left null that the analysis cannot determine, such as a
table's `length`) and `evidence_required`. `unresolved_bank` suggests a
`banks` entry, or a `references` entry per bank holding the address;
`bank_switch` the routine's `switch_banks`; `return_address` its
`inline_bytes` or `no_return`; `jump_table` a `pointers` region; and
`indirect_jump` through tables a `pointers` region, or `low_bytes` and
`high_bytes` regions. Entry points keep what the annotations give them (a
name, a comment). Suggestions are proposals from the code paths that can be
followed; confirm them before applying them.

## Limits

Annotation values must stay within the ROM: ranges within one bank,
`inline_bytes` at most 8192, symbols ending at or below $FFFF. Table values
whose adjusted target would wrap around $FFFF are written as numbers.

ROMs of up to 128 banks of 8 KiB; over MCP, ROM and annotation files of up
to 8 MiB each (also the base ROM). A disassembly is subject to the server's time limit.

The trace remembers up to 32 RAM values and 16 pushed values per path, and
is repeated at most 16 times for joins of paths. A routine's summary follows
up to 4096 instructions over all its paths and calls nested up to 6 deep;
the scan for mapper writes in longer routines follows up to 16384
instructions. A jump table after a JSR has at most 128 entries.

## Queries

`disassemble` returns the first 20 issues; `issues` selects a `kind`,
`offset` and `limit`. `view` returns the source lines of `from`-`to` (default
256 bytes) with their bytes. `xrefs` lists the references to each label,
symbol or address. `routines` lists code reached from each entry without
following calls (also into the next bank), with callers, calls and the RAM
it reads and writes; a routine is followed for up to 16384 instructions and
marked `truncated` beyond. `ram` lists addresses outside the ROM with read
and write counts and every routine that accesses them.

`baseline` (`annotations` or `inline`, with `offset` and `limit`) analyses
other annotations of the ROM, such as a copy of an earlier version, and
returns their `summary` and the issues resolved, introduced and changed
since, matched by `id`. A baseline that cannot be analysed fails the request;
one that names another ROM is a warning.

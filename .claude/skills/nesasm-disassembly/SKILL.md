---
name: nesasm-disassembly
description: Turn an NES ROM (.nes or raw .bin, also a ROM hack against its original) into readable NESASM source that reassembles byte for byte, by iterating with the nesasm MCP server (nesasm-mcp) - disassemble, investigate the code and the author's notes, record confirmed facts with evidence through annotate, re-analyse, then export_disassembly and rebuild. Use this whenever the user wants to disassemble, reverse-engineer or document a NES/Famicom ROM or hack with nesasm, asks to add labels, data regions, entry points or bank references to a *.disasm.json annotation file, to settle disassembly issues (unresolved_bank, indirect_jump, unknown_range...), or to generate or verify disassembly source - even if they only say "continue the disassembly" or name a ROM and a routine.
---

# Iterative disassembly with the nesasm MCP server

The server decodes, traces code and checks round trips; it never calls an LLM.
You supply the judgement: read the real bytes and the user's material, decide
what is confirmed, and record it as annotations with evidence. The aim is
source that is both:

- **reproducible**: it reassembles to the whole input file, header included;
- **classified on evidence**: confirmed routines are code, confirmed tables are
  data, names and comments say what they are and why, and what is not known
  stays unclassified.

A byte-exact round trip does not prove the classification: data decoded as
instructions reassembles to the same bytes. So the round trip is necessary,
and the evidence is what makes the result trustworthy.

The tools (their names may carry a client prefix such as
`mcp__nesasm__disassemble`): `inspect_rom`, `disassemble`, `annotate`,
`query_annotations`, `export_disassembly`, `check`, `assemble`,
`get_reference`. Read `get_reference` with topic `disassembler` once before the
first annotation; it is the authoritative description of every annotation
field, option, issue kind and limit, and this skill does not repeat it. In this
repository the same text is `docs/reference/disassembler.md`, and the Japanese
walkthrough with worked examples is `docs/disassembly-workflow.md`.

## Ground rules

These keep the work correct; each one guards against a mistake that the round
trip cannot catch.

- **Paths are relative to the server root** (`nesasm-mcp --root <dir>`), in tool
  arguments and inside annotations (`rom`, `base`), even when the annotation file
  is in a subdirectory. Everything read or written stays in the root.
- **Never modify the inputs**: the ROM, the original ROM of a hack, patches and
  hand-edited sources. Export to a new directory.
- **Evidence, not decodability.** That a range decodes as instructions is no
  reason to add an entry point or a `code` region. Confirm a caller, a branch, a
  vector or a table that reaches it. Do not force wide unknown ranges to `code`.
- **Do not guess banks.** A bank reference needs the bank switch seen in the
  code: the value written, the mapper write, and that every return path restores
  what the caller expects. A routine's name is not evidence of what it does.
- **Suggestions are not evidence.** Issue `suggestions` are operations derived
  from the traced paths; fill their `needs` (such as a table `length`) from the
  code and the notes, and add your own evidence.
- **Unknown stays unknown.** `.db` covers both confirmed data and unclassified
  bytes; the issues and `regions` tell them apart. Stopping with explained,
  unclassified ranges is a valid result; "no `.db` left" is not the goal.
- **Hacks are separate inputs.** Do not carry addresses, table lengths or
  purposes from the original to the hack (or back) without checking them in
  that ROM.
- **Unconfirmed ideas** go in as entries with `"status": "hypothesis"` and the
  reason in `evidence`. They are kept but not applied (nor exported) unless an
  analysis passes `hypotheses: true`. Confirm them later by upserting the entry
  without `status`, with the added evidence.

## Keep the context small

The annotation file and the responses grow large (a full game reached 1.9 MB of
annotations and responses of hundreds of KB). The tools are built so that you
never need to load them whole:

- **Do not read the `*.disasm.json` file or its log.** Use `query_annotations`:
  it returns the file's `sha256` (the `expected_sha256` for the next `annotate`),
  the counts of each list, the entries a filter selects (`kinds`, `bank`,
  `from`/`to`, `text`, `status`, paged), and the write history with the
  operations and evidence a filter selects.
- **Ask `disassemble` only for what you need.** Query a narrow `view`
  (`bank`, `from`, `to`), the `xrefs` of a few names or addresses, and pages of
  `issues` filtered by `kind`. Pages give `total`: page on with `offset` rather
  than raising `limit` to the maximum. Put the related queries of one step in a
  single call, since each call re-analyses the ROM.
- **Generated operations go in a file.** When a script produces many operations
  (labels from a table, a parsed data format), write them as a JSON array to a
  file in the root and pass `"operations_file": "<path>"` instead of
  `operations`, so they are not written out again as tool arguments.
- **Shared evidence once.** When one observation supports many operations, give
  `"evidence": "..."` beside the operations; it applies to every operation that
  has none of its own. Split calls whose operations rest on different evidence.
- **Brief results.** Pass `"brief": true` to `annotate` in routine iterations: it
  drops the per-bank summaries (the text keeps the totals) and lists in
  `applied` only operations that replaced or removed entries. A written change
  does not repeat its log entry; the log keeps it.
- **No copies of the annotations per iteration.** The log records every write
  with its evidence and SHA-256 before and after; `query_annotations` reads it
  (`history`), and `disassemble` with `baseline` compares against an earlier
  annotation file when one was saved.

## Workflow

### 1. Prepare

1. Confirm the root and the inputs with the user: the ROM (and original ROM for a
   hack), an existing annotation file or none, the material to use (notes,
   specifications, web pages, with their encoding), the goal (which routines or
   data matter), and an output directory distinct from the inputs.
2. `inspect_rom` `{"rom": "roms/game.nes"}`: header, mapper, PRG/CHR sizes,
   vectors, warnings. Note the ROM's size and SHA-256.
3. Create the annotation file (its name ends with `.disasm.json`), or work on a
   copy of an existing one:

   ```json
   {"annotations": "annotations/game.disasm.json",
    "create": {"rom": "roms/game.nes"},
    "operations": [{"op": "set", "kind": "options",
                    "item": {"address_comments": true, "auto_ram_labels": true,
                             "split_banks": true}}]}
   ```

   For a hack, give `"base": "roms/original.nes"` in `create`; for a raw `.bin`,
   `load_address`. Set `mapper4_fixed_prg_16k` only when the code shows MMC3's
   last two PRG banks fixed at $C000/$E000, not from the mapper number alone.

### 2. Take the baseline

Call `disassemble` with the annotations and an `output` path (it writes
nothing; the path names the bank files). Check `success`, `diagnostics`,
`verification.matched` and `mismatch_count` separately: generated source and a
matching round trip are different results. Record the totals from `summary`
(instructions; PRG `code_bytes`, `data_bytes`, `unknown_bytes`; issues by
kind) and `inputs` (SHA-256 of ROM, base and annotations, server version).
Count PRG and CHR apart, and do not call unknown bytes "unanalysed
instructions".

### 3. Pick one open question and read the code

Choose what is closest to the goal: a routine, an issue, a table, a changed
range of a hack. Then gather the facts in one `disassemble` call, for example:

```json
{"annotations": "annotations/game.disasm.json",
 "output": "generated/game.asm",
 "view": {"bank": 30, "from": "$DE90", "to": "$DED4"},
 "xrefs": ["$DEB1", "SetPrgBanks"],
 "issues": {"kind": "unresolved_bank", "limit": 20}}
```

Banks are 8 KiB banks in decimal; addresses are `"$C000"` strings or numbers.
Look at the callers, the bank switch before a call, the RAM a routine reads
and writes (`routines`, `ram`), where an indirect jump's pointer is set, and
where a table ends. For bytes nothing traced, decode a few with `inspect_rom`
(`"disassemble": {"bank": 27, "address": 32768, "length": 32}`, a number
address, `length` in instructions; `hexdump` length in bytes): this shows what
the bytes would be, not that anything executes them.

Check existing annotations and their evidence with `query_annotations` before
adding to them:

```json
{"annotations": "annotations/game.disasm.json",
 "entries": {"bank": 30, "from": "$D800", "to": "$D8FF"},
 "history": {"from": "$D800", "to": "$D8FF", "limit": 5}}
```

Compare with the user's material, and note where each fact comes from.

### 4. Record what is confirmed, with evidence

Choose the annotation that states the fact (details in the reference):

| Confirmed | Annotation |
|---|---|
| name or purpose of a ROM address | `labels`, `comments` (a label alone is no entry point) |
| name of RAM or I/O | `symbols` (with `size` for arrays) |
| reached code entry | `entry_points` |
| data range and format | `regions` (`bytes`, `words`, `text`, ...) |
| jump table | `regions` `pointers` / `low_bytes`+`high_bytes`, `code_targets: true` for code targets, `adjust: -1` for RTS tables |
| bank of a reference | `banks` (where a bank is mapped) and, where needed, `references` with `target_bank` |
| reads from whichever bank is switched in | `references` with `target_window` (a dynamic reference) |
| arguments after a JSR, calls that do not return | `entry_points` `inline_bytes`, `no_return` |
| a routine that maps the bank held in RAM, or returns with the caller's banks | `entry_points` `switch_banks`, `keeps_banks` (only with every return path checked; `switch_banks` also assumes it changes no other RAM the caller knows) |

`references.address` is the address of the instruction that refers, not of
its target. Entries that override the analysis (`references`; entry points with
`no_return`, `inline_bytes`, `switch_banks` or `keeps_banks`; `code` regions;
tables with `code_targets`) and every hypothesis require evidence; give it on
the operation or in the item's `evidence`.

Apply with `annotate`:

```json
{"annotations": "annotations/game.disasm.json",
 "expected_sha256": "<sha256 from the last annotate or query_annotations>",
 "brief": true,
 "operations": [
   {"op": "upsert", "kind": "references",
    "item": {"bank": 30, "address": "$DEB1", "target_bank": 27},
    "evidence": "A=$1B before the call; SetPrgBankPair maps bank A at $8000 and A+1 at $A000 and returns with them (checked on every return path)"},
   {"op": "upsert", "kind": "comments",
    "item": {"bank": 30, "address": "$DEB1", "text": "Calls the start of bank $1B"}}]}
```

- `upsert` replaces the whole entry with the same key; it does not merge. To
  change an entry, take it from `applied[].previous` or `query_annotations`,
  edit it and send it whole. `remove` takes a `key`; `set` changes `options`.
- Write evidence a later reader can check: the addresses and instructions
  observed, the values, and the document and section used. Write explanations
  for readers of the source in `comment` / `comments`, not in evidence.
- Nothing is written unless all operations apply, the annotations are valid,
  the source reassembles (`require_match`, default true) and the file still has
  `expected_sha256`. If it changed, query its `sha256` again rather than
  retrying blindly. Use `dry_run: true` to try a change first.
- Add one cause, or a few related entries, per call, so the effect is clear.

### 5. Check the effect, then repeat

Read `verification`, the totals in the text, and `issues_resolved`,
`issues_introduced` and `issues_changed` (matched by issue `id`). New issues are
not a regression by themselves: they are often open questions of newly reached
code. Suspect the last annotation when new code contradicts its callers, a
table or padding was decoded as code, or `overlap`, `invalid_opcode` or
`code_data_conflict` appear. Fix it (upsert or remove) before going on.

Do not judge progress by the number of instructions or code bytes alone. Repeat
steps 3-5 until the goal is met or no further evidence can be found.

### 6. Hacks

With `base`, the whole hack is traced but the source and the issues cover the
changed ranges, which are included from the original otherwise (`.incbin`).
Issue counts are not comparable with a full disassembly. To see around a
change, analyse a separate copy of the annotations without `base`; to emit a
routine or table around a change as source, add `patches` (they widen the
output; they are not evidence of code).

### 7. Export and rebuild

1. `export_disassembly` with the annotations and a new `output` (for example
   `generated/iteration-02/game.asm`). It writes only when the source
   reassembles, and never overwrites files edited after generation; if refused,
   use another directory instead of deleting the edits. Fixes that annotations
   can express belong in the annotations, so the source stays regenerable.
2. `check` the main `.asm` (`options.raw` for a raw `.bin`).
3. `assemble` it to a path different from every input ROM, and compare the
   whole file with the input (header included) and the SHA-256. For CHR ROM,
   the `.chr` holds all CHR in order. In this repository,
   `tools/trace_rom.py` runs these steps and comparisons together for an iNES
   ROM with CHR and an annotation file at the root (see
   `docs/disassembly-workflow.md`).

### 8. Report

Report the ROM and its SHA-256; the paths of the annotations, log, source and
rebuilt ROM; what was confirmed and on which evidence (call paths, tables,
bank switches); representative before/after excerpts; code, data and unknown
bytes and the issues by kind; the byte and SHA-256 comparison; and the open
questions with what would settle each (valid table IDs, the writer of a RAM
variable, a thread's registration, the runtime bank). A runtime trace
supports the paths it observed, not the ones it did not.

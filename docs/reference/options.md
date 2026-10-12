# CLI options

`nesasm [-options] infile[.asm]`

`-s`/`-S`: segment usage per bank (`-S` adds section ranges); `-l 0..3` or `-l0`..`-l3`: listing level;
`-m`: expanded macro listing; `-raw`: omit iNES header; `-autozp`: select
zero page automatically; `-srec`: produce .s28 instead of .nes; `-e UTF8|SJIS`: encoding;
`-wd`: suppress warnings; `-watch`: rebuild on source/dependency changes;
`-?`/`--help`: help.

UTF-8 is the default on every OS. SJIS uses Windows-932-compatible decoding.
The working directory is searched before NES_INCLUDE directories (semicolon
separated; colons are also accepted outside Windows; at most 10).
Exit status: 0 on success, the error count (up to 255) on assembly errors,
2 on invalid arguments. Output names use the input stem:
.nes, .lst when LIST is enabled, and .s28 when requested. If one of these
files cannot be replaced (for example, another program has it open), none of
them is changed.

Additional Rust options: `--output path`, `--include directory` (repeatable),
`--json` (complete assembly result), `--check` (no file writes).

`--compat-if`: match NesAsmSharp conditional assembly by allowing IF conditions
to change between passes. Each pass evaluates the condition independently.
Forward references in pass 1 produce `W_UNDEFINED_IF` (suppressed by `-wd`);
symbols still undefined in pass 2 remain errors. Without this option, a changed
IF condition is an error. IFDEF/IFNDEF behavior and phase checks are unchanged.
The library and MCP expose this as `options.compat_if` (default `false`).

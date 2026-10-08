# CLI options

`nesasm [-options] infile[.asm]`

`-s`/`-S`: segment usage per bank (`-S` adds section ranges); `-l 0..3` or `-l0`..`-l3`: listing level;
`-m`: expanded macro listing; `-raw`: omit iNES header; `-autozp`: select
zero page automatically; `-srec`: produce .s28 instead of .nes; `-e UTF8|SJIS`: encoding;
`-wd`: suppress warnings; `-watch`: rebuild on source/dependency changes;
`-?`/`--help`: help.

UTF-8 is the default on every OS. SJIS uses Windows-932-compatible decoding.
The working directory is searched before NES_INCLUDE directories (semicolon
separated on Windows, colon separated elsewhere). Output names use the input stem:
.nes, .lst when LIST is enabled, and .s28 when requested.

Additional Rust options: `--output path`, `--include directory` (repeatable),
`--json` (complete assembly result), `--check` (no file writes).

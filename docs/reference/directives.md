# Directives

Directive names are case insensitive and accept a leading dot.

- BANK bank[, name], ORG address: select 8 KiB bank and CPU address.
- PAGE index: select the CPU page (0-7, address index * $2000) without changing
  the bank offset; not allowed inside procedures.
- A word in column 1 is always a label, even when it spells an instruction,
  directive or macro; indent instructions and directives.
- ZP, BSS, CODE, DATA: switch allocation sections.
- DB/BYTE values or strings, DW/WORD values, DS count[, fill]: emit data.
- label EQU expression, label = expression: define a constant.
- INCLUDE "file.asm", INCBIN "file"[, offset[, size]]: include files.
- INESPRG count, INESCHR count, INESMAP mapper, INESMIR flags: iNES header.
- AUTOZP expression: enable automatic zero-page addressing with a nonzero value.
- CATBANK bank: allow data to continue into the next assembler bank.
- BEGINREGION "name", ENDREGION "name": report the byte size of a region.
- `.local .PUBLIC`: expose a local label as `Global.local`.
- MACRO/MAC name ... ENDM: macro body; arguments are `\1` through `\9`.
- IF expression, IFDEF symbol, IFNDEF symbol, ELSE, ENDIF: conditional assembly.
- label FUNC expression: expression function; parameters are `\1` through `\9`.
- RSSET address, label RS size: allocate symbolic storage.
- PROC name ... ENDP, PROCGROUP name ... ENDPROCGROUP: relocate procedures.
- CALL name: emit a procedure call.
- DEFCHR row0,...,row7: eight hexadecimal rows, one per tile row; each row holds
  eight pixels 0 through 3, leftmost pixel in the most significant digit
  (for example `$01230123`).
- Inside a macro, `\#` is the number of the last non-empty argument (0 without arguments).
- A line ending in `\` (outside comments) continues on the next line.
- INCCHR "image.pcx"[, x, y, width, height]: convert PCX to NES tiles.
- LIST/NOLIST, MLIST/NOMLIST: control listing output.
- OPT l+/l-,m+/m-,w+/w-,o+/o-: assembler options.
- FAIL "message": fail the assembly.

## Limits

- ROM: 128 banks of 8 KiB (1 MiB); BANK and CATBANK take 0-127.
- Names: labels, macros and regions up to 64 characters; bank names up to 63.
- Source files: 1 MiB each. Macro expansion and INCLUDE nest up to 32 levels
  together, IF up to 64 levels; at most 1,000,000 lines are processed per pass.
- iNES: INESPRG and INESCHR 0-64, INESMAP 0-255, INESMIR 0-15.
- Addresses: ORG, RSSET and RS results up to $FFFF; ORG below $100 in ZP and
  below $800 in BSS. DS stays below $100 (ZP) or $800 (BSS), and in ROM it must
  fit in the current bank; its fill value is 0-255.
- Data: DB values -128..255, DW values -32768..65535; character literals are one byte.
  With SJIS sources, DB strings may only contain characters SJIS can encode.
- Procedures: up to 8 KiB each, CODE section only, no nesting except PROC inside
  PROCGROUP, and no ORG, PAGE, BANK or section change inside. They are placed in
  the banks after the last code bank, which must exist within the ROM. CALL
  trampolines (18 bytes each) share one bank: at most 455 distinct cross-bank targets.
- INCCHR: PCX images from 16x16 to 1024x768, files up to about 6 MiB.
- Expressions: up to 256 tokens; FUNC calls nest up to 32 levels and are limited
  to 1,000,000 calls per assembly.

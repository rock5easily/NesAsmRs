# Directives

Directive names are case insensitive and accept a leading dot.

- BANK bank[, name], ORG address: select 8 KiB bank and CPU address.
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

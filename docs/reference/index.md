# NESASM reference

Topics: instructions, directives, expressions, options.

NESASM uses 8 KiB assembler banks. iNES PRG counts are in 16 KiB units;
CHR counts are in 8 KiB units. Indent instructions and directives; labels
can start in the first column. A semicolon starts a comment.

```asm
  .inesprg 1
  .ineschr 0
  .inesmap 0
  .inesmir 0
  .bank 0
  .org $8000
Reset:
  sei
  lda #$01
  sta <$00
  jmp Reset
  .bank 1
  .org $FFFA
  .dw Reset, Reset, Reset
```

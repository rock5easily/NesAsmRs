  .bank 0
  .org $8000
emit .macro
  lda #\1
  sta <\2
  .endm
Start:
  emit 1, $00
  emit 2, $01
  rts

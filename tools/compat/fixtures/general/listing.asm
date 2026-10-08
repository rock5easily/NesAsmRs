  .list
  .bank 0
  .org $8000
Start:
  lda #$12
  .db 1, 2, 3, 4
  .dw $1234, $5678
  .defchr $00000011, 0, 0, 0, 0, 0, 0, 0
  rts
  .nolist

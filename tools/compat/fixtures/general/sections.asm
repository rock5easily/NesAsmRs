  .zp
Variable: .ds 2
  .bss
Buffer: .ds 16
  .rsset $400
Temp .rs 4
  .code
  .bank 0
  .org $8000
Start:
  lda <Variable
  sta Buffer
  .dw Temp
  .data
  .org $9000
Data:
  .db 1, 2, 3
  .code
  rts

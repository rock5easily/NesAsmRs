  .bank 0
  .org $8000
VALUE = $1234
SUM FUNC (\1 + \2)
Data:
  .db LOW(VALUE), HIGH(VALUE), (3 + 4 * 5), SUM(2, 3)
  .dw (VALUE << 1), (VALUE & $FF00), (VALUE >> 8)
  .db %10101010, 'A', -1
  .if DEFINED(VALUE)
  .db 1
  .else
  .db 2
  .endif
  .ifdef VALUE
  .db 3
  .endif
  .ifndef missing
  .db 4
  .endif

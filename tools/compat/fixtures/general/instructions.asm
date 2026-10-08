  .inesprg 1
  .ineschr 0
  .inesmap 2
  .inesmir 1
  .bank 0
  .org $8000
Start:
  adc #$12
  and <$34
  asl a
  bcc Next
  bcs Next
  beq Next
  bit $1234
  bmi Next
  bne Next
  bpl Next
  brk
  bvc Next
  bvs Next
Next:
  clc
  cld
  cli
  clv
  cmp $1234,x
  cpx #$12
  cpy <$34
  dec <$34,x
  dex
  dey
  eor [$34,x]
  inc $1234
  inx
  iny
  jmp Start
  jsr Next
  lda [$34],y
  ldx <$34,y
  ldy $1234,x
  lsr a
  nop
  ora $1234,y
  pha
  php
  pla
  plp
  rol <$34
  ror $1234,x
  rti
  rts
  sbc #$12
  sec
  sed
  sei
  sta <$34
  stx $1234
  sty <$34,x
  tax
  tay
  tsx
  txa
  txs
  tya
  .bank 1
  .org $FFFA
  .dw Start, Next, Start

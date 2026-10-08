  .bank 0
  .org $8000
Start:
  call Routine
  call Nested
  rts
Routine .proc
  lda #1
  rts
  .endp
Group .procgroup
Nested .proc
  call Routine
  rts
  .endp
  .endprocgroup

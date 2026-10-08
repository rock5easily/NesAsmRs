	; catbank test asm
	.catbank $00
	catbank $02
	.catbank $04
	catbank $06

	.org $9FFC
	.db 1, 2, 3, 4, 5, 6, 7, 8

	.bank $02
	.org $DFFB
	.db 8, 7, 6, 5, 4, 3, 2, 1

	.bank $04
	.org $9FFA
	.dw $0123, $4567, $89AB, $CDEF

	.bank $06
	.org $DFFB
	.dw $FEDC, $BA98, $7654, $3210

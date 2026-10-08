	; catbank test asm
	.catbank $00

	.bank $00
	.org $9F00
	.incbin "1024bytes.bin"

	.bank $01
	.org $BF00
	.incbin "1024bytes.bin"

	.bank $02
	.org $DF00
	.incbin "1024bytes.bin"

	.bank $03
	.org $FF00
	.incbin "1024bytes.bin"

	.bank $08
	.org $8800
	.incbin "16KB.bin"

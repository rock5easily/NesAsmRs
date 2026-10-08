	; public test asm
	.bank	$00
	.org	$8000
GLabel1:
	lda	#$01
.Valid
	lda	#$02
.Invalid.Local
	lda	#$03

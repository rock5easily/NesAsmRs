	; public test asm
	.bank	$00
	.org	$8000
GLabel1:
	lda	#$01
.LLabel1 .public
	clc
	adc	<$00
	sta	<$00
	rts
GLabel2:
	jsr	GLabel1
	lda	#$03
	jsr	GLabel1.LLabel1
	rts
	.dw	GLabel1.LLabel1
	.db	LOW(GLabel1.LLabel1)
	.db	HIGH(GLabel1.LLabel1)

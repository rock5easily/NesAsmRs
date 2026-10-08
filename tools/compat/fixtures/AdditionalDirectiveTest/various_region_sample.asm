	; beginregion/endregion sample

	.bank $00
	.org $8000
	.beginregion "bank00start-end2"
	.beginregion "bank00start-end1"
	.org $9FFF
	.endregion "bank00start-end1"
	.db 1
	.endregion "bank00start-end2"

	.bank $01
	.org $BF00
	BEGINREGION "bank01-02-region"
	.bank $02
	.org $8888
	ENDREGION "bank01-02-region"

	.bank $04
	.org $9876
	beginregion "subroutine"
	lda <$00
	clc
	adc <$01
	sta <$02
	rts
	endregion "subroutine"

	.bank $00
	.org $8000
	.BEGINREGION "large_region"
	.bank $3F
	.org $FFFF
	.ENDREGION "large_region"

	.bank $0F
	.org $8000
	.beginregion "minus_region"
	.bank $0E
	.org $FFFF
	.endregion "minus_region"


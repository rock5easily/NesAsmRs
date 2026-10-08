	; regionsize() sample

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

	.bank $00
	.org $8000
RS1 = REGIONSIZE("bank00start-end1")
	.db (RS1 & $FF)
	.db ((RS1 >> 8) & $FF)
	.db ((RS1 >> 16) & $FF)
	.db ((RS1 >> 24) & $FF)
RS2 = REGIONSIZE("bank00start-end2")
	.db (RS2 & $FF)
	.db ((RS2 >> 8) & $FF)
	.db ((RS2 >> 16) & $FF)
	.db ((RS2 >> 24) & $FF)
RS3 = REGIONSIZE("bank01-02-region")
	.db (RS3 & $FF)
	.db ((RS3 >> 8) & $FF)
	.db ((RS3 >> 16) & $FF)
	.db ((RS3 >> 24) & $FF)
RS4 = REGIONSIZE("subroutine")
	.db (RS4 & $FF)
	.db ((RS4 >> 8) & $FF)
	.db ((RS4 >> 16) & $FF)
	.db ((RS4 >> 24) & $FF)
RS5 = REGIONSIZE("large_region")
	.db (RS5 & $FF)
	.db ((RS5 >> 8) & $FF)
	.db ((RS5 >> 16) & $FF)
	.db ((RS5 >> 24) & $FF)
RS6 = REGIONSIZE("minus_region")
	.db (RS6 & $FF)
	.db ((RS6 >> 8) & $FF)
	.db ((RS6 >> 16) & $FF)
	.db ((RS6 >> 24) & $FF)

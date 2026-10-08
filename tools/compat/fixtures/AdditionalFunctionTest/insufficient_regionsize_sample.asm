	; regionsize() sample

	.bank $00
	.org $A000
	.beginregion "insufficent_region1"

	.bank $1F
	.org $DFFF
	.endregion "insufficent_region2"

RS1 = REGIONSIZE("insufficent_region1")
RS2 = REGIONSIZE("insufficent_region2")

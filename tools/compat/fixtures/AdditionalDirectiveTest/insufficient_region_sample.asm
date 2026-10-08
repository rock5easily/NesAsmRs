	; beginregion/endregion sample

	.bank $00
	.org $A000
	.beginregion "insufficent_region1"

	.bank $1F
	.org $DFFF
	.endregion "insufficent_region2"

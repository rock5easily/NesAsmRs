	; beginregion/endregion sample

	.bank $00
	.org $8000
	.beginregion "emptyregion1"
	.endregion "emptyregion1"

	.bank $1F
	.org $FFFF
	.beginregion "emptyregion2"
	.endregion "emptyregion2"

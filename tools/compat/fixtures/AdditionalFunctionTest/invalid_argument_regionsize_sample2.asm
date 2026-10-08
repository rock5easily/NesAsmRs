	; regionsize() sample

	.bank $00
	.org $A000
Label1:
REGION_SIZE2 = REGIONSIZE(Label1)
REGION_SIZE4 = REGIONSIZE("")

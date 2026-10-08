	; regionsize() sample

	.bank $00
	.org $A000
REGION_SIZE = REGIONSIZE("dummy_region")
	.db (REGION_SIZE & $FF)

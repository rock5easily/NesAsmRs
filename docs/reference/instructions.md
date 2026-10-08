# Instructions

ADC AND ASL BCC BCS BEQ BIT BMI BNE BPL BRK BVC BVS CLC CLD CLI CLV
CMP CPX CPY DEC DEX DEY EOR INC INX INY JMP JSR LDA LDX LDY LSR NOP
ORA PHA PHP PLA PLP ROL ROR RTI RTS SBC SEC SED SEI STA STX STY TAX
TAY TSX TXA TXS TYA.

Operands: `#$12` immediate, `A` accumulator, `<$12` forced zero page,
`$1234` absolute, `$1234,x`/`$1234,y` indexed,
`[$12,x]` indexed indirect, `[$12],y` indirect indexed,
`[$1234]` indirect JMP. Relative branches must fit -128 through 127.
Without autozp, use `<` to select zero page. `>` forces absolute.
Parentheses group expressions (`jmp ($1234)` is an absolute jump). With autozp,
`($12,x)` and `($12),y` are also indirect; `(addr),y` above zero page becomes
absolute,Y. In immediate operands `#<value` / `#>value` take the low/high byte.
`lda low_byte #$1234` / `lda high_byte #$1234` select the low/high byte
of an immediate value. For memory operands, high_byte increments the address.
`lda.l` / `lda.h` are equivalent to low_byte / high_byte.
`lda [$12].3` inserts `ldy #3` before indirect-indexed loading; `,x++` / `,y++`
append an index increment after an instruction.

The legacy assembler also encodes several 65C02 addressing forms (BIT immediate,
INC/DEC accumulator, indirect zero page and JMP indexed indirect). These forms
are retained for compatibility, although the NES CPU does not execute them as 65C02 instructions.

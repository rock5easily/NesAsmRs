# Expressions

Decimal numbers, `$` or `0x` hexadecimal, `%` binary (`_` may group digits,
as in `%1100_0011`), character literals and symbols are accepted.
Strings may contain `\"`, `\n`, `\r` and `\t` escapes. `*` in operand position is the current CPU address.
Operators: `+ - * / % << >> | ^ & ~ ! = == <> < <= > >=` and parentheses.
Unary `<` and `>` return the low and high byte of a value.
Values use wrapping 32-bit arithmetic; division by zero is an error.

Functions: LOW(value), HIGH(value), BANK(symbol), PAGE(symbol),
DEFINED(symbol), SIZEOF(symbol), VRAM(symbol), PAL(symbol),
REGIONSIZE("region"). These names are also prefix operators with the highest
precedence, so `HIGH label + 1` is `(HIGH label) + 1` and `BANK label` needs no
parentheses. PAGE of a constant is -1. Local labels begin with a dot and belong to the
preceding global label. Published local labels can be referenced as `Global.local`.

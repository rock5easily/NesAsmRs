# Expressions

Decimal numbers, `$` hexadecimal, `%` binary, character literals and symbols
are accepted. `*` in operand position is the current CPU address.
Operators: `+ - * / % << >> | ^ & ~ ! = <> < <= > >=` and parentheses.
Values use wrapping 32-bit arithmetic; division by zero is an error.

Functions: LOW(value), HIGH(value), BANK(symbol), PAGE(symbol),
DEFINED(symbol), SIZEOF(symbol), VRAM(symbol), PAL(symbol),
REGIONSIZE("region"). Local labels begin with a dot and belong to the
preceding global label. Published local labels can be referenced as `Global.local`.

# rpn

Each line of standard input is an expression in reverse Polish notation: tokens separated
by spaces, each an integer (optionally negative, like `-3`) or one of `+ - * /`. For each
non-empty line print its value. Division truncates toward zero. Instead of a value, print
`error: division by zero`, `error: not enough operands`, `error: too many operands` or
`error: bad token TOKEN` (the first bad token) when that happens. Values fit in 64-bit
integers.

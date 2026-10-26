# ledger

Standard input is a list of commands, one per line:

- `open NAME AMOUNT`: open an account with a starting balance (`AMOUNT >= 0`).
- `transfer FROM TO AMOUNT`: move `AMOUNT > 0` from one account to another.
- `show NAME`: print `NAME BALANCE`.

Print `error: no account NAME` for an unknown account (the first one named),
`error: account NAME exists` when opening a name twice, `error: insufficient funds` when
`FROM` holds less than `AMOUNT`, and `error: bad command` for anything else (including
amounts that are not integers or out of range, and a transfer to the same account). A failed
command changes nothing. At the end print `total TOTAL`, the sum of all balances.

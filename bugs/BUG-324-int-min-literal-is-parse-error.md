# BUG-324: Int.MIN literals now parse in expression and pattern position

Status: FIXED
Severity: MED
Area: parser, integer literals, pattern matching, interpreter/compiled parity

## Resolution

The lexer already accepts the `9223372036854775808` magnitude needed for the
source spelling `-9223372036854775808`, encoding it as the wrapped `i64::MIN`
sentinel. Expression parsing handled that sentinel, but pattern parsing still
used checked Rust negation in debug builds, so exact negative patterns and
negative range bounds could panic while parsing.

Pattern parsing now uses the same wrapping negation rule as expression parsing
for negative integer and duration literals, including integer range bounds.

Regression coverage:

- `example_tests::int_min_literal_patterns_work_on_both_backends`


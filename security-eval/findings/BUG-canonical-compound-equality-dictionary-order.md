# Native nested dictionary equality must preserve entry order

Status: found by independent review in the unsubmitted structural-equality
implementation based on `b0204f0e4`. This is a wrong-result compiler defect,
not evidence of memory corruption or a landed regression.

Native compound equality compares dictionary entries in insertion order. The
new comparator preparation incorrectly selected the standard dictionary
`PartialEq` implementation, which instead searches for matching keys.

Reproducer: compare `([left], 0) == ([right], 0)`, where `left` contains
`a: 1, b: 2` and `right` contains `b: 2, a: 1`. The pre-fix CLI parity run
reported interpreter `["false"]`, compiled `["true"]`, and exited 3.

Required correction: prepare native nested dictionaries as ordered pairs,
preserving key-then-value comparisons and custom payload equality. Explicit
source trait calls must retain their declared semantics. Regression coverage:
`rfc0165_recursive_generic::structural_equality_preserves_nested_dictionary_entry_order`.

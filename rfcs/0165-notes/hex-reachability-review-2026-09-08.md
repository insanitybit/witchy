# RFC-0165 hex/reachability review

Scope: combined net diff `f96580ca4^..e91962243`, read-only.  I did not count
removed ancestor test scaffolding as evidence.  No build or test was run.

## Result

No concrete reachability-filtering, hex ABI, or probe-credibility finding in
this slice.

## Reachability filtering

- The root predicate retains `main`, functions under the linked entry prefix,
  and every user-module-qualified function.  Thus it is conservative for
  source-module exported/externally selected functions rather than treating
  only direct calls from `main` as roots
  (`crates/witchy-types/src/pipeline.rs:719-728`).
- The scope walk follows both `Expr::Call` and `Expr::Var` names when they name
  a checked function.  The latter covers function values passed or selected for
  indirect invocation; it is not limited to syntactic direct calls
  (`crates/witchy-types/src/pipeline.rs:740-789`).
- The new pruning only removes a monomorphized instance when the same name is
  an exact checked function body *and* that body is outside the canonical
  scope.  Instances without exact checked bodies remain obligations, so the
  filter cannot convert an unresolved specialization into a successful
  artifact merely by omitting it (`crates/witchy-types/src/pipeline.rs:816-842`).

This conclusion relies on the current function-reference AST representation:
the walk explicitly covers its `Call` and `Var` forms.  A future callable
reference form would need corresponding scope handling before it could be
considered reachable by this rule.

## Hex encoder

- `encoding.hex_encode` accepts exactly one lowered String source and first
  requires its extent.  It rejects every other encoding call in this slice
  (`crates/witchy-lower/src/canonical_lower.rs:7974-8007`).
- Before allocating, the implementation proves `input_len <= (u32::MAX - 4)/2`,
  derives `2 * input_len`, and bounds it by the String representational limit.
  Each loop iteration bounds-checks the source load; output stores are routed
  through the checked byte-store helper (`crates/witchy-lower/src/canonical_lower.rs:8009-8291`,
  `crates/witchy-lower/src/canonical_lower.rs:8404-8531`).  The limit also
  makes the `o`, `o + 1`, and next-output positions non-wrapping for every
  reachable loop iteration.
- The byte is split into masked nibbles and each nibble is mapped only to
  ASCII `0-9` or `a-f`.  Consequently output is valid UTF-8 independent of
  whether the input bytes encode non-ASCII UTF-8.  Allocation returns a fresh
  owned String-sized object; the source is only read and no scalar zero is
  treated as ownership (`crates/witchy-lower/src/canonical_lower.rs:8293-8402`).

## Real std probe credibility and limits

- The positive test imports the real `encoding` std module, compiles the
  checked source through canonical artifact admission, runs both the
  independent canonical interpreter and Wasm, and checks `4869` plus no
  canonical heap leaks.  It is not a synthetic encoder-only unit test
  (`tests/misc/rfc0165_encoding.rs:8-48`).
- The negative test imports and calls real `encoding.hex_decode`, establishes
  that the reachable `Result__map_ok__` specialization has a checked callable
  body rather than a lingering obligation, then requires canonical compilation
  to return `Unsupported`.  This directly exercises the new reachability rule
  against the formerly over-broad generic-instance inventory
  (`tests/misc/rfc0165_encoding.rs:50-83`).
- The probes do not themselves cover empty, non-ASCII, or near-limit input;
  that is a test-coverage gap, not evidence that the emitted bounds or UTF-8
  contract is unsound.

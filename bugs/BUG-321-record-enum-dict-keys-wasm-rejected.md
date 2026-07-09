# BUG-321: Record/enum Dict keys — check passes, interpreter runs, but WASM rejects; RFC-0047 §3's "compiled key support extended to match" is unimplemented, and the codegen reject recommends Float keys the checker bans

Severity: MED
Status: FIXED
Verified: 2026-07-08 fixed on master 8af888b
Component: crates/witchy-lower/src/codegen/mod.rs, RFC-0047, book/src/tour-generics.md, Dict key support, spec §16 parity

## Resolution

The compiled backend now accepts concrete record and enum `Dict` keys with `Eq`.
The regression `compound_dict_keys_work_on_both_backends` covers both a
`derive(PartialEq, Eq)` record key with a `String` field and an enum key with
payload variants. Interpreter and compiled backend output match.

Verification:

```sh
CARGO_TARGET_DIR=target-codex cargo test compound_dict_keys_work_on_both_backends -- --nocapture
```

## Historical Problem

RFC-0047 (status: implemented — contract binding) §3 (`:142-149`): "The Eq bound
moves this whole decision to one type-level rule in the checker: ... Int/String/
Bool/Duration and Eq-deriving records/enums are admissible *in the type system*,
with the compiled backend's key support **extended to match** (record/enum keys
hash their canonical structural form). **Interpreter-only key types drop to
zero**, per the minimize-interpreter-only-features rule." `book/src/tour-generics.md:167`
tells users `derive(Eq)` makes a type "usable as a Set/Dict key".

The compiled key support was never extended. `type K derive(PartialEq, Eq): a:
Int, b: String` used as a dict key passes `witchy check`, runs on the interpreter
(`test ... ok`), but the compiled backend rejects: "could not determine the Dict
key type for WASM; use Int, Float, or String keys (annotate if needed)". Same for
an enum key. Two problems: (1) the check-passes/codegen-fails class RFC-0047 §3
said it was eliminating persists for record/enum keys — an accidental
interpreter-only feature; (2) the diagnostic recommends `Float` keys, which the
checker rejects outright ("`Float` is not a valid `Dict` key — keys require `Eq`,
but `Float` is only `PartialEq`").

Bool and Duration keys (RFC-0047's newly-admissible scalars) DO work compiled, so
only record/enum keys are missing. MED: loud error, not silent divergence, but it
breaks an implemented-RFC contract and ships a misleading diagnostic.

## Repro

```sh
$ W=/Users/cobrien/workspace/witchy/target-claude/release/witchy
$ $W check scratch/ultra-eq/t_reckey.witchy && $W parity scratch/ultra-eq/t_reckey.witchy
t_reckey.witchy: ok
... codegen error: could not determine the Dict key type for WASM; use Int, Float, or String keys (annotate if needed)
$ $W test scratch/ultra-eq/t_reckey_test.witchy
test t_reckey_test.test_record_key ... ok        # interpreter runs it (enum: t_enumkey_test.witchy too)

# controls (parity agree): t_boolkey.witchy, t_durkey.witchy (both newly-admissible scalars work compiled)
# Float-key rejection: t_floatkey.witchy → "`Float` is not a valid `Dict` key — keys require `Eq`…"
```

Probes: `/Users/cobrien/workspace/witchy/scratch/ultra-eq/t_reckey.witchy`,
`t_enumkey.witchy`, `t_reckey_test.witchy`, `t_enumkey_test.witchy`; controls
`t_boolkey.witchy`, `t_durkey.witchy`, `t_floatkey.witchy`.

## Code evidence

- `crates/witchy-lower/src/codegen/mod.rs:5411-5421` — `dict_key_mode` still has
  Float mode 2 and the line-5417 message ("use Int, Float, or String keys"); it
  never handles a record/enum key, so codegen rejects.
- RFC-0047 §3 `:144-149` promises record/enum key support "extended to match" and
  "interpreter-only key types drop to zero"; its Drawbacks lists record/enum dict
  keys as in-scope codegen work.
- `book/src/tour-generics.md:167` still tells users `derive(Eq)` makes a type
  usable as a Set/Dict key.
- Sibling RFC-0047 gap BUG-251 (custom PartialEq in containers) is a different
  codegen surface with a different message — cross-reference, not duplicate.

## Fix direction

Implement compiled record/enum dict-key support in `codegen/mod.rs` (`dict_key_mode`
and the key hashing/compare path): an Eq-deriving record/enum key hashes and
compares by its canonical structural form (the same shape the interpreter uses),
per RFC-0047 §3. Separately, remove the stale `Float` recommendation from the
"could not determine the Dict key type" diagnostic (Float is checker-rejected).
Add a differential test: a `derive(PartialEq, Eq)` record and enum used as dict
keys must run identically on both backends.

# Performance: optimization knobs

`WITCHY_OPT` selects semantics-preserving backend passes for one compilation.
It does not change the language result, accepted normal-mode program, or trap
behavior. `mode opt` is a source-level proof mode; `WITCHY_OPT` is a backend
configuration. They can be used independently.

## Selecting a pass set

```sh
# release is the default when WITCHY_OPT is unset
WITCHY_OPT=release witchy run app.witchy

# debug is the no-optimization reference configuration
WITCHY_OPT=debug witchy run app.witchy

# remove one pass from release for a controlled comparison
WITCHY_OPT=release,-inplace witchy stats bench.witchy

# start with no passes and enable only two
WITCHY_OPT=none,inplace,fold witchy stats bench.witchy
```

`release` and `all` enable the complete hardened registry. `debug` and `none`
disable it. The grammar is comma-separated: `-name` removes a pass and
`+name` adds one. A leading `none,name` creates an allowlist. Presets are only
valid as the first token, and the setting is process-wide for that compiler
invocation.

The 14 registered passes are grouped below by the proof they consume. Every
entry includes a small workload shape, the reason it might not fire, and the
evidence to collect.

## Ownership and representation passes

### `inplace`: uniqueness-driven mutation

**Workload shape:** an unaliased mutable accumulation.

```text
var xs = []
for n in input:
    xs.push(n)
```

`inplace` reuses a list, string, dictionary, record, or related write-back
buffer when uniqueness analysis proves that no other live value can observe the
old storage. `own` and `move` can carry the same ownership token across a call.
With `-inplace`, the copy-correct path re-owns at each update.

An alias, active reference loan, unknown escaping function, or unsupported
projection defeats the proof only at the affected operation. Compare `reowns`,
`heap_bytes`, and `extract_copied_bytes`. A read-only helper summarized as
`let` can remain in the chain; an opaque function value cannot.

### `views`: confined borrowed slices

**Workload shape:** select a large slice and consume it before the owner is
used again.

```text
var records = load_records()
let window = records.slice(10, 10010)
consume(window)
records.push(last)
```

`views` keeps the selected elements as a confined view instead of materializing
a new slice. The view cannot escape through a return, closure, task, channel,
or suspension point. `-views` is the copy oracle. Compare `heap_bytes` and
`packed_alloc_*`, while checking that output and owner reuse remain identical.

This backend pass does not invent a lifetime. The source checker must first
prove the lifetime and owner relation in opt mode.

### `sroa`: scalar replacement of aggregates

**Workload shape:** create a tuple or record, use its fields, and let it die in
the same scope.

```text
for i in 0..n:
    let point = (i, i + 1)
    total = total + point.0 + point.1
```

`sroa` replaces the non-escaping aggregate with scalar locals. Returning it,
putting it into an owned container, passing it through an unsupported boundary,
or taking an unknown alias keeps the ordinary heap representation. Compare
`heap_bytes` on a large loop; a counter-neutral result can still be confirmed
by inspecting generated Wasm.

### `rc-elide`: same-shape destination reuse

**Workload shape:** repeatedly assign compatible list or record shapes to one
confined local.

```text
var row = [0, 0, 0, 0]
for i in 0..n:
    row = [i, i + 1, i + 2, i + 3]
```

`rc-elide` reuses a compatible destination when the old buffer is not
observable. It grows retained capacity when needed and overwrites fixed record
slots. An alias or self-referential assignment forces a fresh value. Compare
`heap_bytes` and allocation counters with `WITCHY_OPT=release,-rc-elide`.

### `rc-floor`: free-at-overwrite reclamation

**Workload shape:** replace a confined value with a newly produced value whose
shape is not known to be the same literal shape.

```text
var state = parse(seed)
for input in inputs:
    state = advance(state, input)
```

`rc-floor` returns the dead, non-aliased allocation to a size-classed free list
at overwrite. A later compatible allocation can reuse it. It is broader than
`rc-elide`, which specializes compatible same-shape assignments. Compare
`rc_free_calls`, `rc_reuse_calls`, `rc_reused_bytes`, and `live_cells`.

## Code-shape passes

### `fold`: constant folding and propagation

**Workload shape:** compile-time literals and immutable literal bindings.

```text
let header = "Witchy " + "book"
let width = 3 * 7 + 1
emit(header, width)
```

`fold` evaluates constant arithmetic, comparisons, boolean and bitwise
expressions, unary literals, string concatenation, and immutable literal
bindings. `-fold` leaves those operations for runtime. It preserves wrapping
integer and IEEE floating-point behavior, so a runtime coincidence is never
treated as proof of constancy.

### `direct-call`: closure devirtualization

**Workload shape:** bind one closure and call it repeatedly without reassigning
the binding.

```text
let add_one = fn(x: Int) -> Int: x + 1
for x in input:
    total = total + add_one(x)
```

`direct-call` can turn the known closure call into a direct Wasm call. Any
reassignment, indirect storage, trait use, uncertain capture, or host crossing
keeps `call_indirect`. Compare generated Wasm or codegen diagnostics; heap
counters may not change.

### `bounds-elide`: counted-loop bounds proof

**Workload shape:** index a list with the exact counter range derived from its
unchanged length.

```text
var i = 0
while i < xs.length():
    total = total + xs[i]
    i = i + 1
```

The pass removes the repeated bounds check only when it can prove that `xs` is
not reassigned and the counter covers the valid range. A dynamic index, a
mutated list, or a shape miss retains the check. Compare emitted Wasm and keep
the interpreter/Wasm trap-parity fixture in the evidence.

### `closure-elide`: non-escaping closure environment

**Workload shape:** call a closure directly in the scope that creates it.

```text
let offset = 10
let add_offset = fn(x: Int) -> Int: x + offset
total = total + add_offset(value)
```

`closure-elide` passes immutable captures as lambda arguments instead of
allocating an environment. Escaping storage, reassignment, indirect calls,
trait use, and host boundaries retain the boxed environment. Confirm the shape
with heap counters plus generated code; speed alone is not evidence that the
environment disappeared.

### `loop-unroll`: four-lane counted loops

**Workload shape:** a long, predictable counted loop over fixed-layout data.

```text
var i = 0
while i < xs.length():
    total = total + xs[i]
    i = i + 1
```

`loop-unroll` emits four lanes plus a guarded scalar tail when the range and
layout are proven safe. `-loop-unroll` retains one scalar body. Long hot loops
may benefit; short loops can lose to code size and compile time. Compare matched
cold and warm timings, output, and bounds/trap behavior.

### `direct-storage-var`: direct `var` write-back

**Workload shape:** a whole local is passed to a `var` function and no live alias
or projection overlaps it.

```text
fn normalize(var text: String) -> Nil:
    text = text.trim()

var text = input
normalize(text)
```

`direct-storage-var` commits the returned value directly into the caller's
proven-disjoint slot. A field projection, live loan, alias, callee escape,
representation mismatch, or evaluate-twice hazard keeps staged reconstruction.
Compare `direct_storage_var_accesses` and verify write-back behavior.

## Layout and post-processing passes

### `unbox`: packed and unboxed layouts

**Workload shape:** a list of fixed scalar records scanned in a hot loop.

```text
type Point packed:
    x: Int
    y: Int

let points = [Point(1, 2), Point(3, 4)]
```

`unbox` selects a canonical flat layout for packed records, tuples, lists, and
fixed-layout sums. Generic, closure, trait, worker, channel, rendering, and
host boundaries without an exact physical ABI fail closed or use the ordinary
representation. Compare `packed_alloc_calls` and `packed_alloc_bytes` with
`WITCHY_OPT=release,-unbox`. See [Layouts and allocation lifetime](appendix-performance-layout.md)
for the declaration and boundary rules.

### `region`: arena and loop-watermark reclamation

**Workload shape:** parse or format many temporary values and return only a
small summary.

```text
region -> String:
    let parsed = parse(line)
    summarize(parsed)
```

`region` rewinds region-born temporaries at a proven scope or loop iteration.
Values that leave the region are copied out when necessary. A yielded value,
unknown escape, or frame that outlives the region prevents the rewind. Compare
`heap_bytes`, `region_rewind_calls`, and `region_copy_bytes`.

### `wasm-opt`: Binaryen post-processing

**Workload shape:** a cold Wasm compile where post-processing cost matters.

```sh
WITCHY_OPT=release witchy build --wasm app.witchy
WITCHY_OPT=release,-wasm-opt witchy build --wasm app.witchy
```

`wasm-opt` runs Binaryen's post-pass over emitted Wasm when the tool is
available. The result is cached, so compare cold and warm builds separately.
If Binaryen is absent, the pass is a graceful no-op. It is not an ownership or
semantic switch.

## Pass interaction

Passes are intentionally layered. A `unique` accumulation can expose a shape
for `inplace`; `packed` can expose fixed strides for `bounds-elide` and
`loop-unroll`; `fold` can make `sroa` easier; `region` can reclaim the
temporaries left by a shape that did not qualify for scalar replacement.

Disable one pass at a time when diagnosing a regression. If a pass is enabled
but its proof does not match, the correct result is the ordinary representation,
not a forced transformation. Use [Measuring and diagnosing](appendix-performance-measurement.md)
to record the source shape, pass setting, counters, generated-code evidence,
and parity result together.

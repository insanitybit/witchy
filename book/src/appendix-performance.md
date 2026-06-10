# Appendix: Performance — the Ownership Knobs

The parameter conventions from [the functions chapter](tour-functions.md) are
not just a correctness model — they are witchy's **optimization knobs**. This
appendix says exactly what each one buys you and where.

First, the ground rule: witchy has value semantics. A callee must never be able
to mutate what the caller still observes. Each backend honors that its own way —
the interpreter and the WASM backend through their value representations, and
the **native (Rust) backend** by choosing, per parameter, between *clone*,
*borrow*, and *move*. That choice is the knob.

| You write | The native backend emits | Cost profile |
|---|---|---|
| `fn f(xs: List(Int))` *(default)* | the argument is **cloned at the call site** so the caller keeps its value | safe everywhere; a deep copy per call for collections (`List`/`Dict`/`String`/records). Scalars (`Int`, `Float`, `Bool`, `Duration`) and capabilities are `Copy` — free |
| `fn f(let xs: List(Int))` | the parameter lowers to **`&T`** and the call passes a reference — **no clone** | the read-only fast path. The compiler enforces the borrow can't escape (not returned, stored, or mutated), which is what makes eliding the clone sound |
| `fn f(own xs: List(Int))` / `sink` | the argument is **moved — no clone** | for "I'm consuming this": the callee may take buffers apart in place; the checker forbids the caller from using the value afterwards, so nothing needs copying |
| `fn f(inout n: Int)` | a mutable write-back parameter | mutate-in-place with the final value delivered back to the caller's `var`; no copy-out |

A few practical consequences:

- **The default is "correct first."** If you annotate nothing, every call is
  value-semantic and safe; for collection arguments on a hot path you pay one
  clone per call.
- **`let` is the free win.** A function that only *reads* a collection should
  take it `let`. Same observable behavior, zero-copy on native:

  ```witchy
  fn sum(let xs: List(Int), i: Int) -> Int:
      if i >= length(xs):
          0
      else:
          at(xs, i) + sum(xs, i + 1)

  fn main(console: Console):
      print(console, int_to_string(sum([1, 2, 3, 4], 0)))
  ```

- **`own` + `move` ends a value's story.** When the caller is finished with a
  value, transferring it lets the callee reuse the allocation instead of copying
  it — and use-after-move is a compile error, not a latent bug.
- **Closures are cheap to pass** (reference-counted on native), and the checker
  forbids cloning what can't be cloned — you don't manage any of that.

Two honest caveats. First, these knobs change *performance on the native
backend*; on the interpreter and the WASM backend they are checked for exactly
the same semantics but compile to the same code either way — so a program tuned
with `let`/`own` is no faster in the sandbox, just equally correct. Second, the
usual advice applies: write the default first, and reach for the knobs when a
profile (or an obvious hot loop over a big collection) says so. The signature
documents the decision either way — that's the point of putting ownership in the
type.

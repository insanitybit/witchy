# WIR — the Witchy Intermediate Representation

A design for the compiled backend's representation. This document describes
what WIR *should be*, why, and how to get there. Nothing here is implemented
yet — it is the plan that the incremental refactor in
[oracle-only-migration.md](oracle-only-migration.md) §"Future refactors — A"
sketched at a high level, worked out concretely and grounded in the current
`src/codegen.rs`.

The interpreter (`src/interpreter.rs`) is **not** part of WIR. It stays a
tree-walking evaluator over the AST and remains the differential oracle. WIR is
purely the compiled backend's internal form. The parity discipline
([architecture.md](architecture.md)) is what validates every WIR change.

---

## 1. Recommendation up front

**Paradigm: a structured, tree-shaped, typed IR — *not* an SSA-over-CFG IR.**
WIR keeps wasm's nested control-flow constructs (`block` / `loop` / `if` /
labeled `br`) as first-class IR nodes rather than lowering to a basic-block CFG.
It is closest in spirit to **Binaryen IR** (an IR that is "essentially a subset
of WebAssembly"), specialised to witchy's value model.

**Level: mid-level, witchy-typed, value-tree** — one rung above raw wasm
instructions. WIR instructions are typed expression nodes (`MakeList`,
`FieldGet`, `Index`, `Binary(op, ty)`, `Call`, `MatchTag`, `ToSlot`/`FromSlot`,
…) that *carry the witchy type* they operate on, sitting over an explicit,
expression-tree control-flow spine. They are not yet wasm opcodes; a final,
mechanical `WIR → wasm-encoder` pass turns them into bytes. This is "start where
codegen already operates, lift over time": today's `compile_expr` already emits
a value-stack expression tree with type-directed choices, so WIR is a
*reification* of the data structure codegen builds implicitly in `String`s, not
a new abstraction layer.

**The core justification — the structured-control-flow decision.** The single
hardest problem when compiling to wasm is that wasm has *structured* control
flow (reducible, nested, no arbitrary `goto`), while conventional optimizing
IRs (LLVM, Cranelift, Rust MIR) are SSA over an arbitrary CFG. Lowering a CFG
back to wasm requires a **relooper / Stackifier / control-flow-recovery** pass
(Ramsey's algorithm in WAFFLE, the Relooper in Binaryen/Emscripten,
`WebAssemblyFixIrreducibleControlFlow` in LLVM) — a known-hard transformation
that can duplicate blocks, introduce a dispatch-variable state machine for
irreducible loops, and generally *degrade* code it cannot prove reducible.

witchy never needs that pass, and WIR is designed to keep it that way.
**witchy's source control flow is already structured and reducible by
construction**: the surface language has only `if` / `while` / `for` / `match`
/ `return` / `break` / `continue` — there is no `goto`, no computed jump, no
labelled multi-entry loop. `src/codegen.rs` already exploits this: `match`
lowers directly to nested `block $a … br_if` shapes (codegen.rs ~3517),
`if`/`while`/`for` map one-to-one onto wasm's `if`/`loop`+`br`, and every
`compile_*` function returns a `String` of *already-structured* wasm. Adopting
an SSA-CFG IR would mean **destroying** this structure (AST → CFG) only to pay a
relooper to **rebuild** it (CFG → structured wasm) — taking on the single
hardest piece of wasm codegen to solve a problem witchy does not have. A
structured IR keeps the gift the source language already gives us.

The cost we accept for this choice: classical SSA-based optimizations (global
GVN, sparse conditional constant propagation, aggressive code motion across
arbitrary control flow) are *less* natural over a structured tree than over
SSA. That trade is right for witchy, because (a) the highest-value
witchy-specific optimizations — the in-place/ownership pass, redundant
slot-conversion elimination, devirtualizing `call_indirect` to direct calls,
non-escaping aggregate flattening — are **type/shape-directed local or
intraprocedural rewrites**, not CFG-global SSA passes; and (b) the heavy
SSA-class optimization is *already available downstream for free* from
Cranelift (wasmtime compiles WIR's wasm output through Cranelift's SSA mid-end)
and, optionally, the Binaryen `wasm-opt` post-pass. WIR's job is to host the
optimizations only witchy can do (because only witchy has the types and the
uniqueness facts); it should not try to out-Cranelift Cranelift on the
optimizations the engine already does well.

In one sentence: **WIR is a typed, structured, value-tree IR — a witchy-aware
Binaryen-IR — because witchy's structured source flow means a CFG/SSA IR would
buy us a relooper we'd rather never write.**

---

## 2. Concrete WIR shape

A Rust sketch. Names are indicative, not final. The guiding constraints: it
must express everything `compile_module` emits today; it must carry witchy types
inline (the current `TypeTable`/`Facts` are keyed by `*const Expr`/`*const Stmt`
pointer identity, which **cannot survive** a lowering to new nodes — so the type
and uniqueness facts that codegen reads from side-tables must be *baked onto*
WIR nodes during lowering); and it must lower to wasm with no control-flow
recovery.

### 2.1 Module / function

```rust
pub struct WirModule {
    pub types:     Vec<WirFuncType>,      // dedup'd wasm signatures
    pub imports:   Vec<WirImport>,        // host capability functions (authority!)
    pub funcs:     Vec<WirFunc>,
    pub table:     Vec<FuncRef>,          // closure / call_indirect entries
    pub globals:   Vec<WirGlobal>,        // actor state, __witchy_reowns, watermarks
    pub memory:    MemorySpec,            // min/max pages
    pub data:      Vec<DataSegment>,      // interned strings, const blobs
    pub exports:   Vec<WirExport>,        // main, __region_copy_bytes, __witchy_reowns, ...
    pub start:     Option<FuncIdx>,
}

pub struct WirFunc {
    pub name:    String,
    pub params:  Vec<WirLocal>,           // declared locals carry a WirTy
    pub ret:     Vec<WirTy>,              // multi-value (own-ABI returns 2)
    pub locals:  Vec<WirLocal>,           // body-introduced locals, pre-collected
    pub body:    WirSeq,                  // the structured spine (below)
    pub facts:   FnFacts,                 // uniqueness facts, lowered onto nodes
}

pub struct WirLocal { pub id: LocalId, pub ty: WirTy, pub name: Option<String> }
```

### 2.2 The structured control-flow spine

This is the crux. Control flow is **nested nodes**, never a `Vec<BasicBlock> +
terminators`. The variants mirror wasm 1:1, so lowering is a direct walk.

```rust
/// A statement-level node. Executes for effect and/or leaves a typed value.
pub enum WirNode {
    /// Straight-line sequence (a wasm instruction run; no control transfer).
    Seq(WirSeq),

    /// `if (cond) { then } else { els }` — wasm `if`/`else`/`end`.
    /// `result` is the block's value type (None for a statement `if`).
    If { cond: Box<WirExpr>, then_: WirSeq, els: WirSeq, result: Option<WirTy> },

    /// A labelled wasm `block` — the target of a forward `Br`.
    Block { label: Label, result: Option<WirTy>, body: WirSeq },

    /// A wasm `loop` — the target of a back-edge `Br` (`continue`/while).
    Loop { label: Label, body: WirSeq },

    /// `br`/`br_if` to an *enclosing* Block/Loop label (always reducible:
    /// labels only ever name a lexically-enclosing node — this invariant is
    /// what guarantees no relooper is ever needed).
    Br { target: Label, cond: Option<Box<WirExpr>> },

    /// A `match` lowering: scrutinee in a temp, arms as nested Blocks with
    /// MatchTag/pattern tests and Br-on-fail. (Structurally just Block+Br+If,
    /// kept as a node so a pass can see the match shape.)
    Match { scrut: Box<WirExpr>, arms: Vec<WirArm>, result: WirTy },

    /// Evaluate an expression for its side effect, drop its value.
    Drop(Box<WirExpr>),

    /// Bind / rebind a local (lowered `let`/`var`/`assign`). The `reown`
    /// field carries the in-place pass's verdict (see §3.1), baked on here.
    SetLocal { local: LocalId, value: Box<WirExpr>, reown: ReownKind },

    Return(Option<Box<WirExpr>>),
}

pub type WirSeq = Vec<WirNode>;
```

### 2.3 The typed expression layer (the mid-level, witchy-aware part)

Expression nodes are typed and *witchy-semantic* — high enough that a pass
"knows" it is indexing a list of `Int` vs. chasing a raw pointer.

```rust
pub enum WirExpr {
    // --- leaves ---
    ConstI64(i64),                 // Int / Duration
    ConstF64(f64),                 // Float
    ConstI32(i32),                 // Bool, ptr, erased-capability placeholder
    StrConst(DataIdx),             // pointer to interned [len|bytes]
    GetLocal(LocalId),
    GetGlobal(GlobalIdx),

    // --- value-model conversions (THE redundant-conversion optimization target) ---
    ToSlot(Box<WirExpr>, Kind),    // value -> universal i64 slot
    FromSlot(Box<WirExpr>, Kind),  // universal i64 slot -> value
    KindCast(Box<WirExpr>, Kind, Kind),

    // --- arithmetic / logic, typed ---
    Binary { op: BinOp, ty: NumTy, lhs: Box<WirExpr>, rhs: Box<WirExpr> },
    Unary  { op: UnOp,  ty: NumTy, arg: Box<WirExpr> },

    // --- aggregates (boxed, length-prefixed; the witchy heap model) ---
    MakeList   { elem: WirTy, items: Vec<WirExpr> },
    MakeTuple  { slots: Vec<WirExpr> },
    MakeRecord { name: TypeName, fields: Vec<WirExpr> },
    MakeCtor   { ctor: CtorId, tag: u32, fields: Vec<WirExpr> },
    Index      { base: Box<WirExpr>, idx: Box<WirExpr>, elem: WirTy }, // bounds-checked
    FieldGet   { base: Box<WirExpr>, offset: u32, ty: WirTy },
    MatchTag   { base: Box<WirExpr> },                                 // read ctor tag

    // --- strings ---
    StrConcat(Box<WirExpr>, Box<WirExpr>),
    StrEq(Box<WirExpr>, Box<WirExpr>),

    // --- structural equality / render (per-shape helpers) ---
    EqShaped { shape: EqShape, lhs: Box<WirExpr>, rhs: Box<WirExpr> },
    Render   { shape: EqShape, arg: Box<WirExpr> },

    // --- calls ---
    Call        { target: FuncIdx, args: Vec<WirExpr> },        // direct
    CallIndirect{ table_idx: Box<WirExpr>, sig: TypeIdx, args: Vec<WirExpr> },
    CallHost    { import: ImportIdx, args: Vec<WirExpr> },      // capability authority

    // --- in-place accumulation primitives (carry the reown verdict) ---
    ListPushCap   { list: Box<WirExpr>, elem: Box<WirExpr>, cap: LocalId, inplace: bool },
    DictInsertCap { /* ... */ inplace: bool },
    StrAppendCap  { /* ... */ inplace: bool },

    // a control-flow expression that yields a value (if/match in value position)
    Control(Box<WirNode>),
}
```

### 2.4 Types in WIR

Two layers, deliberately:

* **`WirTy`** — the *witchy-level* type, retained so passes are shape-aware.
  It is the lowered `Ty`/`ast::Type` projected onto what the backend cares
  about:

  ```rust
  pub enum WirTy {
      Int, Float, Bool, Duration, Str, Nil,
      Capability,                       // erased -> i32 handle/placeholder
      List(Box<WirTy>),
      Tuple(Vec<WirTy>),
      Record(TypeName),
      Adt(TypeName, Vec<WirTy>),
      Dict(Box<WirTy>, Box<WirTy>),
      Fn(Vec<WirTy>, Box<WirTy>),       // closure -> table-index record
      Slot,                             // the universal untyped i64 slot
  }
  ```

  `WirTy` is exactly what `EqShape`/`ValType`/`ty_list_nesting` reconstruct
  *ad hoc* from the AST today — but computed **once at lowering** and attached
  to nodes, instead of re-derived from side-tables (`local_shape`,
  `local_list_elem_valtype`, `fn_ret_tuple_slots`, the ~20 `local_*` maps in
  the `Codegen` struct). This collapse of the side-table bag is one of the
  biggest contributor-facing wins.

* **`Kind`** — the *wasm-level* representation (`I32` / `I64` / `F64`), the
  existing `codegen::Kind`. Every `WirExpr` has a `kind()` derivable from its
  `WirTy`. This is the layer `ToSlot`/`FromSlot`/`KindCast` operate on and the
  layer the final lowering reads to pick opcodes (`i64.add` vs `f64.add`).

Keeping *both* is the answer to the "type representation" nuance: the high
layer lets passes reason about `List(Int)` vs raw pointer; the low layer drives
the untyped-slot value model and opcode selection. They are linked by
`fn kind_of(ty: &WirTy) -> Kind` (today's `ty_kind`).

### 2.5 Worked example

Source:

```
fn sum(xs: List(Int)) -> Int:
    var total = 0
    for x in xs:
        total = total + x
    total
```

WIR (sketch; `t#` locals, slot conversions explicit so a pass can cancel them):

```
func sum(params: [xs: List(Int)]) -> [Int]
  locals: total:Int(i64), __i:i32, __n:i32, x:Int(i64)
  body:
    SetLocal total = ConstI64(0)
    SetLocal __i   = ConstI32(0)
    SetLocal __n   = ListLen(GetLocal xs)
    Block $exit:
      Loop $head:
        Br $exit if !(GetLocal __i  <  GetLocal __n)         # i32 lt_s
        SetLocal x = FromSlot(Index{ base: xs, idx: __i, elem: Int }, I64)
        SetLocal total = Binary{ Add, I64, GetLocal total, GetLocal x }   # reown: NA (scalar)
        SetLocal __i = Binary{ Add, I32, GetLocal __i, ConstI32(1) }
        Br $head
    Return(Some(GetLocal total))
```

Note the `FromSlot(..., I64)` on the list read: because the list element type is
`Int`, the slot decode is `i64.reinterpret`-free (`Int` already *is* i64), so
**redundant-conversion elimination** (§3.2) cancels a `ToSlot`/`FromSlot` pair
if `Index` is later fed straight into an `i64` consumer. Over a `List(Float)`,
the same pass would *keep* the `f64.reinterpret_i64`. The pass sees this because
the `WirTy` is on the node, not inferred from a pointer-keyed side-table.

---

## 3. How witchy's optimizations map onto WIR

The thesis: WIR is **"one place"** for a compiled-backend optimization. Today an
optimization is woven into ~630 `push_str`/`format!` sites across 9.3k lines; a
contributor must understand the string-emission contract to add one and risks a
parity divergence in raw WAT. With WIR, an optimization is a
`fn(&mut WirModule)` pass registered in a pass pipeline — typed, testable in
isolation, and unable to emit malformed wasm.

### 3.1 The in-place / ownership pass (the acid test)

This is the question the brief flags: *can the in-place optimization be a clean
WIR pass instead of being woven into WAT emission?* **Yes — and the cleanest
form keeps the analysis where it is and consumes it during lowering.**

Today (`src/analysis.rs`) the uniqueness pass runs over the **lowered AST**,
producing `Facts` keyed by `*const Stmt` identity; `src/codegen.rs` reads
`Facts` at emission time to choose `$list_push_cap` (in-place) vs the copying
path and to thread the `${name}__cap` shadow token. Two clean options, and the
recommendation picks the first:

* **(Recommended) Analyze-then-lower.** Keep `analysis::analyze` exactly as is
  (it already operates on the form both backends share, which the interpreter
  oracle also needs). At **AST → WIR lowering**, query `Facts` for each
  self-assign site and **bake the verdict onto the WIR node**: `SetLocal {
  reown }` and `ListPushCap { inplace }` carry a resolved `ReownKind`
  (`InPlace` / `ForcedCopy` / `ReownThenInPlace`). The `*const Stmt` pointer
  problem disappears because the query happens *while we still hold the Stmt*,
  before it is dropped. WIR then needs no pointer-keyed side-table; the fact is
  intrinsic to the node. The `__cap` shadow local becomes an explicit
  `WirLocal` and the cap-token dataflow becomes ordinary `GetLocal`/`SetLocal`
  on it — which means later WIR passes (DCE, copy-prop) can even *see and
  simplify* the token plumbing that is invisible string today.

* **(Alternative) Re-run uniqueness on WIR.** Port the share/dirty lattice to
  operate on `WirNode`/`WirExpr` (which have explicit `SetLocal`/`GetLocal`).
  This is "more SSA-like" and would let the pass *also* fire on accumulator
  shapes that only appear after lowering (e.g. inside an inlined callee). It is
  more work and risks a second source of truth diverging from the interpreter's
  in-place guard. **Defer it**: do analyze-then-lower first; only port to WIR if
  inlining-created accumulators prove worth it.

Either way, the `WITCHY_NO_INPLACE` forced-copy differential and the
`__witchy_reowns` counter (the existing soundness nets) carry over unchanged —
they become "lower with all `inplace=false`" vs "lower with the real verdict,"
a one-line toggle in the lowering, and the no-oracle metamorphic check
(`examples_agree_under_inplace_and_forced_copy`) still guards it.

### 3.2 Redundant slot-conversion elimination (the headline new win)

The value model's `ToSlot`/`FromSlot` (`i64.extend_i32_s`,
`i64.reinterpret_f64`, `i32.wrap_i64`, …) are pervasive and frequently cancel:
storing into a slot then immediately reading it back, or a generic ABI
round-trip on a concretely-typed value. Today they are emitted as inline
strings and **cannot be cancelled** — the mid-end (Cranelift) sees them as real
`reinterpret` ops and keeps them. On WIR they are explicit nodes with known
`Kind`s, so a peephole pass is trivial and local:

```
FromSlot(ToSlot(e, k), k)            => e
ToSlot(FromSlot(e, k), k)            => e
KindCast(e, a, a)                    => e
FromSlot(Index{elem: Int}, I64)      => Index{...}        # Int slot is already i64
```

This is the optimization that *justifies WIR existing* (the brief's #1 driver):
it is impossible over strings and natural over a typed value-tree, and it
directly attacks witchy's most pervasive overhead.

### 3.3 Inlining, CSE, DCE

* **DCE** — a `Drop` of a pure `WirExpr`, an unread `SetLocal`, an unreachable
  arm after a constant `MatchTag`: standard structured-tree DCE (a post-order
  walk with a liveness set), made safe because WIR marks which `CallHost`/`Call`
  nodes are effectful (capability calls, `fail`) and which `WirExpr`s are pure.
* **CSE** — hash-cons pure `WirExpr` subtrees within a `WirSeq` (and across
  dominating Blocks); reuse a local. Pointer-free value equality on the typed
  tree makes this clean. Witchy benefits specifically from CSE of repeated
  `ListLen`, `FieldGet`, and `EqShaped`/`Render` helper calls.
* **Inlining** — splice a small callee's `WirFunc.body` into the caller,
  renaming locals (the existing `alpha_rename`/`Renamer` logic in codegen
  already does the AST analog and ports directly). Inlining is what unlocks
  devirtualizing `CallIndirect` → `Call` for the `list.map(xs, fn(x): …)` shape
  (a goal in [performance.md](performance.md) Phase 2) and exposes more
  slot-conversion cancellation across the call boundary.

All four are independent `fn(&mut WirModule)` passes in a fixed pipeline; a
contributor adds one by writing a pass and a unit test that lowers a snippet,
runs the pass, and asserts on the resulting WIR (or on `WIR → WAT` text) — no
need to understand the other 9k lines. **That is the "one place."**

### 3.4 What stays downstream (deliberately not in WIR)

Register/local allocation across arbitrary control flow, global value
numbering, instruction scheduling, and machine-specific lowering are
**Cranelift's** job (wasmtime runs WIR's wasm output through it) and optionally
Binaryen's (`WITCHY_WASM_OPT=1`, already wired in `runtime::optimize_module`).
WIR does the witchy-semantic, type-and-ownership-directed rewrites the engine
*can't* do because it has thrown the types away; it does not duplicate the
engine's general SSA optimization.

---

## 4. Lowering WIR → wasm (`wasm-encoder`)

`wasm-encoder` `0.251` is **already in the dependency tree** (via `wat`), so no
new dependency. The lowering is `fn encode(module: &WirModule) -> Vec<u8>`,
emitting the binary sections directly (types, imports, funcs, table, memory,
globals, exports, code, data). It replaces `compile_module -> String` +
`wat::parse_str`.

### 4.1 Control-flow emission — no relooper, ever

Because WIR control flow is **already** nested `Block`/`Loop`/`If`/`Br`, with
the invariant that **a `Br` target is always a lexically enclosing label**,
emission is a direct structural walk:

```
WirNode::If{cond,then_,els,result} =>  enc(cond); If(blocktype(result));
                                       enc(then_); Else; enc(els); End
WirNode::Loop{label,body}          =>  Loop(empty); enc(body); End      # label = depth
WirNode::Block{label,result,body}  =>  Block(blocktype(result)); enc(body); End
WirNode::Br{target,cond}           =>  [enc(cond);] Br/BrIf(rel_depth(target))
```

Labels resolve to **relative branch depths** by tracking the nesting stack
during emission (the standard wasm requirement). There is **no CFG, no
dominator tree, no Ramsey/Relooper/Stackifier, no `FixIrreducibleControlFlow`**
— the structured invariant is preserved end-to-end from the AST, which is the
entire payoff of the §1 paradigm choice. (If a future witchy feature ever
introduced irreducible flow — it has none today and none planned — *that*
feature would owe a relooper; the IR does not.)

### 4.2 Locals and the value stack

WIR is value-tree shaped, so it maps onto wasm's operand stack the way codegen
already does: post-order emission of a `WirExpr` leaves exactly one value on the
stack (the current `compile_expr` contract). Locals are used where the source
needs a name or a value is consumed more than once:

* **Declared locals** (`WirFunc.locals`) are pre-collected at lowering (today's
  function-header scan that emits `(local $x i32)`), so the encoder just emits
  the locals vector. The `${name}__cap` tokens, `MATCH_TMP`, `TUPLE_TMP`,
  `TRY_TMP`, and per-loop watermark/call temps become ordinary `WirLocal`s.
* **Stackification (minimize locals)** is an *optional* WIR→wasm peephole: a
  `SetLocal t; …; GetLocal t` with a single, adjacent use and no intervening
  effect can drop the local and leave the value on the stack. This is a small,
  contained win; Cranelift largely redoes local coalescing anyway, so it is
  low priority — but it lives cleanly as one pass rather than being a property
  of how strings happened to be concatenated.

### 4.3 The value model

`Kind` (I32/I64/F64) and the `ToSlot`/`FromSlot`/`KindCast` semantics are
**unchanged** — WIR reifies them as nodes instead of strings, and §3.2
optimizes them. Boxed aggregates (length-prefixed records, `[len|cap|slots]`
lists, `[len|utf8]` strings), the bump arena, loop-watermark resets, and
`region:` copy-out keep their exact runtime representation; WIR nodes
(`MakeList`, `Index`, region-scoped sequences) lower to the same heap layout and
the same `$list_push_cap` / `__region_copy_bytes` helpers. WIR is a change of
*compiler representation*, not of *runtime ABI* — which is what keeps parity
intact.

### 4.4 Capability-import preservation (safety)

Authority is "which host functions the module imports"
([capabilities.md](capabilities.md), architecture.md §"WASM value model"). In
WIR this is explicit and *more* auditable than today: `WirModule.imports` is the
authority set, and a capability operation is a `CallHost{ import }` node — there
is no other way to reach a host function. The footprint analyzer
(`src/capabilities.rs`) continues to recompute authority **from source** (never
from the module), so emission cannot widen authority. Capabilities stay erased
to i32 placeholders/handles (`ConstI32` for `Console`/`Clock`/`Env`; a handle
index for `Dir`/`Net`). The invariant "paths/allowlists never enter guest
memory" is a property of the host functions in `runtime.rs`, untouched by WIR.
A useful new check the typed IR enables: assert that **every** `CallHost`'s
`import` is in the granted set at encode time — a defense-in-depth lint that the
string backend can't express.

### 4.5 Structural-equality and render helpers

`EqShape` and the per-shape memoized helper generation move onto WIR almost
verbatim: `EqShaped{shape}` / `Render{shape}` nodes name the shape; a lowering
pass (the analog of today's helper synthesis) emits one `WirFunc` per distinct
shape id and rewrites the node to a `Call` to it. The "unresolvable shape →
loud compile error" rule (the project's anti-silent-pointer-compare invariant)
is preserved: lowering a `Render`/`EqShaped` whose `EqShape` can't be resolved
is a `CodegenError`, exactly as today.

---

## 5. Prior-art table

| System | Control-flow model | How it reaches wasm | What witchy borrows / avoids |
|---|---|---|---|
| **Binaryen IR** | Structured AST-like IR, "essentially a subset of wasm"; nested block/loop/if; parallel per-function codegen. Optionally accepts a CFG and reloops it. | Trivial — IR *is* wasm-shaped; serialize directly. | **Borrow the core idea**: a wasm-shaped structured IR is the cheapest, most parity-safe target. WIR = Binaryen IR specialised to witchy types + the slot model + uniqueness facts. Keep `wasm-opt` as the existing optional post-pass, not a dependency. ([Binaryen README](https://github.com/WebAssembly/binaryen)) |
| **Binaryen Relooper / ReReloop** | CFG → structured shapes (Simple/Multiple/Loop). | The canonical relooper. | **Avoid needing it.** witchy's source is already structured; designing WIR to *keep* that means we never run a relooper. ([Relooper.h](https://github.com/WebAssembly/binaryen/blob/main/src/cfg/Relooper.h)) |
| **Cranelift IR (CLIF)** | SSA over a CFG; φ as **block parameters**, not phi-instructions; can be irreducible. | It's the *engine's* IR; wasmtime lowers wasm→CLIF→machine code. | **Don't reimplement it — stand on it.** WIR emits wasm; Cranelift then does the SSA-class optimization for free. Borrow *block-parameters-not-phis* only if WIR ever needs SSA locally (it does not in v1). ([cranelift ir.md](https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/docs/ir.md)) |
| **WAFFLE** | SSA-CFG with blockparams, *plus* a wasm backend (rare). Reducifier (block duplication) + Ramsey's algorithm for structured recovery. | Reducifier → control-flow recovery → wasm. | **Cautionary tale**: it's exactly the SSA-CFG-to-wasm path, and it must carry a Reducifier + Ramsey recovery to get back to structured form. Validates the decision to *not* go SSA-CFG. Worth reading if witchy ever ingests external wasm. ([waffle](https://github.com/bytecodealliance/waffle)) |
| **LLVM wasm backend** | SSA-CFG; `WebAssemblyFixIrreducibleControlFlow` + CFGStackify. | Fix-irreducible (dispatch-variable state machine) then stackify. | **Avoid**: the dispatch-variable rewrite for irreducible loops is precisely the complexity a structured IR sidesteps. ([FixIrreducible](https://cs6340.cc.gatech.edu/LLVM8Doxygen/WebAssemblyFixIrreducibleControlFlow_8cpp.html)) |
| **RVSDG** | Acyclic hierarchical regions; data-flow-centric; structured control implicit; state edges sequence effects. | Used by wasm optimizers because regions ≈ structured control (no relooper). | **Philosophically aligned, too heavy for v1.** RVSDG's region nesting is the rigorous version of WIR's structured spine, and its state-edge idea is a clean way to model effect ordering. **Avoid** the full demand-dependence graph + SSA-region machinery now (large build cost, and Cranelift already does the data-flow heavy lifting downstream); revisit as a *future* mid-end if witchy wants aggressive code motion. ([RVSDG paper](https://arxiv.org/abs/1912.05036), [jlm](https://github.com/phate/jlm)) |
| **MoonBit** | Multi-level ANF IRs (Core → MCore → CLambda), whole-program opt, then a thin lowering per backend. | One short lowering step from CLambda to wasm. | **Borrow the layering instinct** (analyze/optimize at a higher level, thin final lowering) — WIR is witchy's CLambda-equivalent. ANF is an alternative shape; witchy's value-tree is fine because its source is already near-ANF after lowering passes. ([MoonBit wasm](https://www.moonbitlang.com/blog/first-announce)) |
| **Grain** | Mid-level IRs (incl. an ANF `mashtree`); compiles **via Binaryen**. | Emits Binaryen IR; Binaryen serializes. | **Confirms the pattern**: a small language compiling to wasm leans on a wasm-shaped IR + Binaryen. witchy emits its own binary (no Binaryen dep) but the structured-IR shape is the same bet. ([Grain](https://serokell.io/blog/grain-with-oscar-spencer)) |
| **Rust MIR** | CFG of basic blocks + terminators; simplify-CFG, inlining, const-prop, DCE passes. | Lowers to LLVM IR (not wasm-structured). | **Borrow the *pass discipline*** (a pipeline of small, independently-tested `fn(&mut Body)` passes — `SimplifyCfg`, `ConstProp`, `Inline`) as the model for WIR's pass registry. **Avoid** its basic-block CFG *shape* (that's the relooper trap for wasm). ([rustc MIR](https://rustc-dev-guide.rust-lang.org/mir/)) |
| **Swift SIL** | SSA-CFG, two-stage (raw/canonical), ownership (OSSA) modeled in-IR. | Lowers to LLVM IR. | **Borrow the idea of modeling ownership *in the IR*** — SIL's OSSA is the precedent for baking witchy's uniqueness/reown verdict onto WIR nodes (§3.1). **Avoid** the SSA-CFG substrate. |
| **Go SSA** | SSA-CFG, ~40 small ordered passes, great for contributors. | Architecture-specific lowering. | **Borrow the contributor ergonomics**: many tiny, ordered, individually-testable passes is the gold standard WIR's "one place" should imitate. **Avoid** the CFG substrate for the wasm reason. |

The consistent signal: every SSA-CFG system that targets wasm pays a
control-flow-recovery tax (Relooper / Ramsey / FixIrreducible+Stackify). Every
system that *starts* wasm-shaped (Binaryen, Grain, RVSDG-regions) avoids it.
witchy's structured source lets it start wasm-shaped — so WIR does.

---

## 6. Phased plan

The honest reality (from
[oracle-only-migration.md](oracle-only-migration.md) §A): converting the
recursive `compile_*` core is a **near-total rewrite of the emission layer**
(~630 emission sites, ~9.3k lines), and it is **atomic per function** — you
can't half-convert `compile_expr`, because it recurses into itself. The mitigations
that make this safe rather than terrifying are (a) the interpreter oracle +
parity sweep, (b) a `WIR → WAT` printer asserted **byte-for-byte equal to the
current WAT** during migration, and (c) the existing no-oracle metamorphic
checks (`assembled_binary_runs_like_the_wat`,
`examples_agree_under_inplace_and_forced_copy`, forced-copy diff, `fmt`
idempotence). Strategy: **introduce WIR as a new sink that reproduces today's
WAT exactly, flip over once it's byte-identical, then optimize.**

### Milestone 0 — the data structures + the WAT printer ✅ DONE

Added `src/wir.rs` with the §2 types (`WirModule`/`WirFunc`/`WirNode`/`WirExpr`/
`WirTy`/`Kind`/`BinOp`) and a `WIR → WAT` pretty-printer (`to_wat`). No lowering;
codegen untouched. Five hand-written WIR functions round-trip through
`wat::parse_str` to runnable wasm and produce the expected output: integer
arithmetic, a value-`if`, the `sum`-style loop spine (`Block`/`Loop`/`Br` — the
structured-control paradigm validated end to end), `ToSlot`/`FromSlot` cancellation
fodder, and a string `print` (data segment + `StrPtr` + `Load` + a void `CallHost`).
**Exit met:** WIR exists, round-trips to runnable wasm, zero codegen changed; full
suite green.

(M0 uses *names* for locals/labels/funcs — the WAT printer + `wat` crate resolve
them; relative branch depths + indices arrive with the binary encoder in M3.)

### Milestone 1 — lower the leaf/expression layer 🟡 IN PROGRESS

Convert `compile_expr`'s expression arms (literals, `Var`, `Binary`, `Index`,
`FieldGet`, `Call`, `ToSlot`/`FromSlot`, `MakeList`/`MakeTuple`/`MakeRecord`) to
build `WirExpr` instead of `String`. The control spine (`compile_block`,
`compile_match`, `if`/`while`/`for`) stays string-based and splices in printed-WIR
fragments via `wir::expr_to_wat`.

**Done so far** (green, full suite + 6 `wir` round-trips passing):
- the bridge `wir::expr_to_wat` (a flat 4-space fragment printer matching
  codegen's style);
- a `lower_expr(&Expr) -> Option<WirExpr>` helper in `codegen.rs` that
  `compile_expr` tries first, falling back to the legacy arms on `None` — the
  mechanism that grows WIR coverage while the tree stays green;
- the leaf arms (`Int`/`Float`/`Duration`/`Bool`/`Str`, plain-local `Var` guarded
  by `is_plain_local_var`) and **`Unary`** (`not`/`neg`/`bitnot`, plus the
  value-neutral `move`/`await`) now lower through `WirExpr`, **byte-identical**
  to the former inline WAT (`WirExpr::Unary` + `UnOp` added, with a round-trip
  test covering the `0 - x` and `x ^ -1` expansions). `ConstF64` prints via plain
  `{x}` Display to match codegen's `Expr::Float` (the `wat` crate infers f64 from
  the mnemonic — no forced `.0`);
- value-neutral **`As`** (`e as T`, capability narrowing / ascription) forwards to
  the inner expression, exactly like the legacy arm;
- **`Binary`** fully: numeric/bitwise (`+ - * / %`, `& | ^ << >>`), comparisons
  (`== != < <= > >=`), and **mixed-kind** operands via a `WirExpr::Convert` node
  that reproduces `kind_convert` exactly (only `i32<->i64` emit; everything else,
  including any `f64` pair, is a no-op like the original). The genuinely bespoke
  cases correctly return `None` → legacy: string concat, string/compound/dict
  compares, generic-reference compares, and float **ordering** (the NaN-trapping
  `$f_*` helpers). Guards `operand_is_compound` / `is_generic_ref_compare` mirror
  the legacy predicates.
- **`&&`/`||`** short-circuit via the new `WirExpr::Control(Box<WirNode>)` node —
  a value-`if` (`a && b` → `if a { b } else { 0 }`; `a || b` → `if a { 1 } else
  { b }`). This required making the WIR control printer **flat** (control bodies
  print at the same depth as their `if`/`else`/`end` keywords), matching codegen's
  uniform 4-space layout — the prerequisite for byte-identity on *any* control
  flow (M1's remaining control-in-value arms and all of M2).
- **`Field`** access (tuple element `pair.0`, record field `rec.name`): both read
  an i64 slot at `base + 4 + 8*idx`, recovered at the field's kind —
  `FromSlot(Load{Add(base, off), I64, 0}, kind)`. (Also fixed `to_slot_op`'s I32
  case to sign-extend `i64.extend_i32_s`, matching codegen's `to_slot`/`kind_convert`
  — it was a latent `_u`; `FromSlot` was already correct.)
- **`Try`** (`e?`, non-`inout` functions): stores the operand in the `$TRY_TMP`
  scratch, then a value-`if` on the tag — take the success payload (`tmp+4`) or
  early-`return` the whole Err/None. Modelled with the new `WirExpr::Seq(WirSeq)`
  node (a node sequence that leaves one value — `SetLocal; value-If`). The `inout`
  epilogue variant (`cur_fn_inout_params` non-empty) stays in legacy.
- **aggregate constructors** (`List`, `Tuple`, `Ctor`): no `Store` node needed —
  each is `header; elems-in-slots; call $mkN` (the `$mkN` helper allocates). Lowered
  via `lower_aggregate` to `Call{ "mkN", [ConstI32(header), ToSlot(elem, k)…] }`
  (header = length / `0` / ctor tag), recording `mk_arities`. These emit the first
  **explicit `ToSlot` nodes** in real lowered code — the M4 slot-elimination pass's
  raw material.
- **plain user-function `Call`** (no own-ABI token, no `inout`): the soundness
  problem (a user fn can't be told from a bare builtin by name) is dodged by
  converting **inside `compile_call`'s `_ =>` fallback** — only reached after every
  builtin/native/closure is excluded by the dispatch, so it's sound *by
  construction*, no predicate needed. `try_lower_user_call` lowers each arg, widens
  it to the param kind (`Convert`), and emits `Call{ name, args }`. The own-ABI /
  `inout` / closure (`call_indirect`) variants stay in legacy.
- **`RecordUpdate`** (bare-variable base): rebuilds the record as `Call{ "mkN",
  [ConstI32(tag), per-field…] }` where each field is either the overridden value in
  a slot (`ToSlot`) or the base's raw slot copied across (`Load{Add(base, off)}`,
  no `from_slot`). A non-`Var` base needs the scratch-local pool → stays in legacy.
- **builtin `Call` arms — via a `lower_call(name, args)` dispatch** tried before
  `compile_call`'s legacy match (precedence preserved — each arm tests the same
  name/arity). **~60 arms converted**: `crypto.*`, `compiler.*`, `regex.*`,
  `encoding.*`, the `string.*` family (`to_int`/`starts_with`/`ends_with`/`split`/
  `chars`/`from_code`/`contains`/`index_of`/`replace`/`trim`/`to_upper`/`to_lower`/
  `substring`), `get_env`, `now`, `print`, `int_to_duration`/`duration_to_int`, the
  `list.*` ops (`push`/`at`/`concat`), the whole `dict.*` family (`new`/`keys`/
  `values`/`pairs`/`size`/`insert`/`get_or`/`has`/`remove`/`update` — key-mode
  side-operand + slot conversions; `dict_key_mode` errors fall through to legacy via
  `.ok()?`), and the net/dir/build host ops (`read`/`list`/`subdir`/`exists`/`is_dir`/
  `write`/`append`/`make_dir`/`recv_*`/`send_*`/`accept`/`connect`/`listen`/`restrict`/
  `close`/`read_build`/`write_out`/`reply`). Helpers used: `call`(guest module fn),
  `host`(`_host` import → `CallHost`), `nil0`(void effect then `i32.const 0`). All set
  the same `uses_*`/`used_*` flags. Byte-identical across `pm`/`coven`/`coven_client`,
  `word_freq`/`wordcount` (dict), `collections`/`sorting` (list), `files` (dir),
  `http` (net), and the string/print corpus.
  - **Deferred** (need a small new node): `string.length`/`list.length`
    (`i64.extend_i32_u`, unsigned), `math.sqrt`/`to_int`/`to_float` (f64 ops), `fail`
    (`unreachable`), and the scratch/multi-branch arms (`__render`, `char_count`,
    `try_connect`, `send`/`ask`).

**Byte-identity check (repeatable):** temporarily add `return None;` to the top of
`lower_expr`, then diff `witchy emit-wat <prog>` before/after against a corpus.
Confirmed **identical** on `02_types_arith`, `49_numbers`, `27_edge_cases`,
`08_fizzbuzz`, `13_sorting`, `26_rpn`, `03_records_enums`, `07_iterators` (the
float programs cover `ConstF64`; `sorting`/`fizzbuzz` cover integer comparisons).
(The standing correctness gate is the behavioral parity suite: compiled output vs
the interpreter oracle.)

**Approach (resolved):** the recursion that blocks inline per-arm conversion is
handled by `lower_expr` recursing into *itself* for sub-expressions, threading
`Option` (a composite arm is `Some` only if every sub-expression is) — so the set
grows bottom-up without ever touching `compile_expr`'s `-> Result<String>`
signature. The legacy `match` is the shrinking fallback; an arm retires from it
once `lower_expr` covers the case. Control-in-value-position rides the
`WirExpr::Control(Box<WirNode>)` node: `&&`/`||` are lowered; `if`/`match`/`block`
*expressions* follow once their branch **blocks** can be lowered (that block
lowering is the M2 work, so these wait for it).

**Remaining for M1**:
- **`Call` — the rest of the cluster** (extend `lower_call`): the remaining
  builtin/native arms (`encoding.*`, `string.*`, list/dict, net, dir, `print`, …;
  many add a trailing `i32.const 0`, an arg `kind_convert`, or call a `_host`
  import → `CallHost`), plus the user-call **own-ABI / `inout` / closure
  (`call_indirect`)** variants. (The crypto/compiler/regex cluster and the plain
  user call are done.) `MethodCall`/`Apply` lower to `Call` before codegen;
- **`Lambda`** — closure materialisation (table slot + capture record);
- **`if`/`match`/`block` expressions** — `WirExpr::Control`/`Seq`, but their branches
  are **blocks**, so they wait on the M2 block-lowering.

(Lowered so far: leaf arms incl. `Float`, plain-local `Var`, `Unary`, the whole
`Binary` arm, `As`, `&&`/`||`, `Field`, `Try` (non-`inout`), the `List`/`Tuple`/
`Ctor` constructors, `RecordUpdate` (bare-var base), the **plain user-function
`Call`**, and ~60 builtin `Call` arms (see the `lower_call` bullet above). Infra in
place: the `Control`, `Seq`, `Convert`, `ToSlot`/`FromSlot`/`Load`/`Call`/`CallHost`
nodes, the flat control printer, and `seq_to_wat`.)

> **`Binary` cases that stay in legacy** (each returns `None`; documents *why* the
> numeric WIR path can't claim them):
> - **float ordering** (`<`/`<=`/`>`/`>=` when the common kind is `f64`) compiles
>   to NaN-trapping helpers `call $f_lt`/`$f_le`/… and sets `uses_float_ord` — it is
>   **not** `f64.lt`, so `WirExpr::Binary`'s `f64.lt` mnemonic would diverge;
> - **`Concat`** and `Add` where either operand's val-type is `Str` (→ `$concat`);
> - **`and`/`or`** short-circuit (lower to a value-`if`, not a wasm `and`/`or`);
> - **string compare** `==`/`!=`/ordering on `Str` operands (`$str_eq`/`$str_cmp`);
> - **compound `==`/`!=`** (structural eq helper) and the loud rejects for compound
>   ordering, `Dict` `==`, and generic-reference compares.

Validate behaviorally against the legacy path (the parity + metamorphic suites;
byte-identical *text* is impractical because codegen's WAT is flat-string and WIR is
structured — §7.3 relaxation), plus the repeatable `emit-wat` diff above and focused
`wir` round-trips per node. **Exit:** the whole expression layer flows through
`WirExpr`; behavior unchanged; parity green.

### Milestone 2 — lower the control spine; whole functions are WIR 🟡 STARTED

Convert `compile_function` / `compile_block` / `compile_match` /
`compile_region` to produce `WirNode` trees. Now a whole `WirFunc` exists. Bake
the uniqueness verdict (§3.1, analyze-then-lower) onto `SetLocal`/`ListPushCap`
at this step. Still assert byte-identical WAT via the printer. **Exit:** the
entire compiled backend is `AST → WIR → (printer) → WAT → wasm`, byte-identical
to today, parity green. The `Codegen` side-table bag (the ~20 `local_*` maps)
collapses into `WirTy`-on-nodes here — the big contributor-facing simplification.

**Done so far** (all byte-identical across the corpus incl. `pm`/`coven`, full
suite green):
- `lower_block` lowers a block whose statements are `Let`/`Expr`/`Return`/
  `LetTuple`/`Break`/`Continue`/`Assign` (in a function with no `inplace_push` var /
  `inout` param / own-ABI param) to a `WirSeq` rendered by `seq_to_wat`.
  `Break`/`Continue` → `Br` to the enclosing `loop_labels` (so loops *with* early
  exit now lower); `LetTuple` → store-once + per-name `FromSlot(Load(tmp+4+8*i))`;
  `Assign` → `SetLocal{name, Convert(value, vk, target)}` for the simplest case only
  (a plain local that is NOT a self-assign shape, a string/list state field, or a
  global — those keep their in-place fast paths / site-accounting / `global.set` in
  legacy). Subtlety handled: `take_kills` bumps a non-idempotent kill counter, so all
  statements are pre-lowered first and `take_kills` runs only once everything is
  known to lower (never doubled by a fallback).
- **`Expr::If`** (value-if, with and without `else`) and bare **`Expr::Block`** lower
  their branches through `lower_block` → `WirExpr::Control(If{..})` / `Seq`. Subtlety:
  codegen lowers the branch blocks BEFORE the cond (with `else`) but cond-first
  (no `else`); `intern` assigns string offsets in call order, so the lowering order
  is matched exactly. Mixed branch kinds handled by `convert_block_tail` (wraps the
  block's tail `Push` in a `Convert`).
- **`Expr::While`** (no-watermark variant) → `Seq([Block{Loop{ br_if-exit,
  Drop(body), br-back }}, Push(0)])`. Watermark empty-condition checked read-only;
  `next_label` allocated in codegen's order and restored on a bail.
- **`Expr::For`** (general list iteration, non-range, no-watermark) →
  `Seq([SetLocal(list), SetLocal(idx,0), Block{Loop{ idx>=len→br_if exit; bind
  var=FromSlot(Load((list+4)+idx*8)); Block(fc){Drop(body)}; idx+=1; br-back }},
  Push(0)])`. Same watermark guard + `next_label` save/restore; preserves the
  `elem_record_type_of → local_records.insert(var)` side-effect so `var.field`
  resolves in the body. The range-counting **`for x in lo..hi`** also lowers (i64
  counter + bound; inclusive ranges add the pre-increment `ctr == end` guard).

- **`Expr::Match`** (scalar patterns — `Wildcard`/`Int`/`Bool`/`Var`) →
  `Seq([SetLocal($MATCH_TMP, ToSlot(scrut)), Block $d(result T){ per-arm Block $a{
  Br($a, !cond); binds; (Br($a, !guard)); Push(convert(body)); Br($d) }; Unreachable
  }])`. Added a `WirNode::Unreachable` (printer + encoder). `next_label` restored on
  a bail; intern order (scrutinee first, then per-arm guard/body) matched.
  `lower_pattern` handles `Wildcard`/`Int`/`Bool`/`Var` and **`Tuple`** (recursive:
  element `i` = the i64 slot at `FromSlot(value,i32)+4+8*i`; the AND of sub-conditions
  via `wir_and_chain` — the WIR twin of `and_chain`, nested short-circuit value-`if`s);
  `List`/`Ctor`/`Str` patterns still return `None` → those matches stay in legacy.
  Byte-identical across `12_patterns`/`16_json`/`rpn`/`pm`/`coven`/`coven_client`.

**Remaining:** `Match` non-scalar patterns (tuple/list/ctor/string + `..rest`/
guards over them), the non-simple `Assign` variants (in-place / field / global),
then `compile_function → WirFunc` and `compile_module → WirModule` — at which point
M3 can flip the sink (after the prelude-blob work above).

> **M3 obstacle — the hand-written WAT helpers (researched; plan chosen).**
> `compile_module_with` (codegen.rs ~7450) assembles a WAT string in this order:
> closure `(type $clos{n})`s → ~53 `(import "witchy" …)` (the ONLY imports, gated by
> `uses_*`/`used_*`) → memory → table+elem (if lambdas) → data + `$heap`/region
> globals + helper funcs → user funcs → lifted `$__lam{i}` → generated eq/ts/rcopy
> helpers → the `run` export; then `wat::parse_str`. The helpers are ~75 functions /
> ~2.2k WAT lines: ~65 `*_WAT` consts (strings/lists/`$dict_*`/`$crypto_*`/host
> wrappers) + Rust-generated `$mk{n}` and per-shape `$eq_*`/`$ts_*`/`$rcopy_*` (built
> via `format!` into a `BTreeMap<String,String>`). They reference `$heap`/
> `$__witchy_reowns` globals, the table + `call_indirect (type $clos1)`, multi-value
> `(result i32 i32)`, and call each other / `_host` imports densely.
>
> **Chosen approach (option b → a): pre-compile a prelude blob, splice, evolve.**
> Compile all gated static + generated helpers into ONE wasm **prelude blob once**
> behind a lazy static (uses `wat` only at compiler-build time / first use — OFF the
> per-program hot path, satisfying "`wat` out of the build pipeline"). Reserve a
> **fixed prelude block at the FRONT of every index space**: imports `[0..53)`, then
> prelude funcs at fixed indices, then user `WirFunc`s after — so `Call{name}`
> resolves by name to a unified index for both spliced and encoded funcs. **Encoder
> work first:** grow `wir_encode` with Global, Table, Element sections + multi-value
> results + a raw-body splice path (wasm-encoder `Function`/raw bytes) that relocates
> the blob's func/type/global indices into the unified space; add `table`/`globals`
> fields to `WirModule`. Helpers can migrate to real `WirFunc`s later (option a) for
> M4 — without blocking the M3 sink-flip. This keeps the textual helpers byte-verbatim
> (parity-safe).

### Milestone 3 — flip the sink to `wasm-encoder` (binary, no WAT) 🟡 PIPELINE BUILT & PROVEN; FLIP BLOCKED ON THE PRELUDE

**Done — the AST → WIR → binary pipeline exists end-to-end and is oracle-proven.**
- `src/wir_encode.rs` `encode(&WirModule) -> Vec<u8>` (wasm-encoder 0.251): sections
  Type/Import/Function/Table/Memory/Global/Export/Element/Code/Data; multi-value;
  raw-body splice path; func index space = imports then defined funcs; type dedup;
  local name→index; `Br` relative depths. **`$clos0..MAX_CLOS` are reserved at type
  indices `0..MAX_CLOS` FIRST**, so spliced prelude `call_indirect (type $closN)`
  bodies validate (the prelude was assembled clos-types-first).
- `codegen::assemble_wir_module` builds the whole `WirModule` — prelude raw-body
  helpers (in the documented index order) + lowered user `WirFunc`s + the `run`
  export + imports/globals(`$heap`,`$__witchy_reowns`)/data/table — or `None` when
  any reachable function does not fully lower (→ WAT fallback). User functions are
  captured as a side effect of `compile_function` (`assemble_wir_func`, gated by
  `collect_wir`), reusing all its setup. `compile_module_binary` = assemble +
  `wir_opt::optimize` + `encode` + `wasmparser::validate`.
- `lower_expr` gained an `Expr::Call` arm (gated by `collect_wir` so the WAT path
  keeps `compile_call`'s full dispatch + byte-identity); user calls discriminated by
  an exact `emitted_funcs` set (never an intrinsic/native like `math.sqrt`).
- Proven by `wir_binary_path_runs_and_agrees_with_oracle`: programs compiled
  straight to binary run identically to the interpreter oracle AND the WAT path.

**Blocked — the flip cannot become the default yet, for TWO prelude reasons:**
1. **Capability model.** The static prelude is "all features on": every binary
   module imports the full host surface, including authority fns (`crypto.sign`,
   dir/net/env). A minimal (e.g. print-only) program then can't instantiate under
   its real, minimal capability grant. witchy's whole point is that a program's
   imports = its authority, so the binary path must import ONLY what the program's
   footprint uses.
2. **`wat` removal.** The prelude blob is itself compiled via `wat::parse_str`
   (lazily, in `wir_prelude`), so even after a flip the `wat` crate stays in the
   build. The criterion needs it GONE.

The raw-body prelude bakes import/func/`call_indirect` INDICES, so it can't be
pruned incrementally (removing a helper shifts every later index). **Decided fix
(unifies both blockers): lower the prelude HELPERS to WIR.** Then the encoder
re-indexes by name → emit only the reached helpers+imports (capability-correct,
unblocks the flip) AND no `wat` in the prelude (drops the crate). Needs new WIR
nodes for the bulk-memory/memory ops the helpers use (`memory.copy/fill/grow/size`)
+ translating the ~64 `*_WAT` helpers. Tracked as task #35; prereq for the flip.

**Remaining for the flip:** #35 (prelude → WIR) → `lib::compile_source` tries
`compile_module_binary` first → once every construct lowers, drop `wat = "1"` from
`Cargo.toml` and delete the WAT sink. Keep `WIR → WAT` printer as `witchy emit-wat`.
**Exit:** no WAT text in the pipeline; `wat` crate out of the build.

### Milestone 4 — turn on optimizations (the actual payoff) 🟡 SLOT-ELIMINATION SHIPPED; MEASURABLE WIN PENDING LOWERING

Add the pass registry (`fn(&mut WirModule)` pipeline) and land passes in
ascending risk: **redundant slot-conversion elimination** (§3.2, the clearest
win) → **DCE** → **CSE** → **`CallIndirect`→`Call` devirtualization** →
**inlining**. Each pass: a unit test that lowers a snippet, runs the pass,
asserts on resulting WIR; plus the corpus differential (optimized vs
unoptimized output must match — a new metamorphic check in the family of
`examples_agree_under_inplace_and_forced_copy`).

**Done:** `src/wir_opt.rs` `optimize(&mut WirModule) -> OptStats` — redundant
slot-conversion elimination (`FromSlot(ToSlot(x,k),k)` / `ToSlot(FromSlot)` /
identity `Convert(k,k)`), fixpoint, skips raw-body funcs. Unit-tested on synthetic
redundancy (node reduction asserted); integrated into `compile_module_binary`
before encoding; behavior-preservation proven against the oracle
(`wir_slot_elimination_is_behavior_preserving`).

**Key finding — no measurable win yet.** The pass eliminates **0 nodes** on every
program that currently lowers: the redundant `FromSlot(ToSlot)` round-trips it
targets arise at **generic/monomorphization and closure boundaries** (args pushed
as i64 slots, immediately consumed as typed) — exactly the constructs that DON'T
lower yet (they hit the WAT fallback). The measurable payoff therefore depends on
the M2 lowering tail reaching those constructs. Until then the pass is correct but
inert on real programs.

**Remaining:** the **in-place / ownership pass** (§3.1, the second headline win) is
NOT built. DCE/CSE/devirt/inlining not built. Plus the lowering tail above, which
is what actually surfaces the slot-elimination win. **Exit:** contributors have
"one place" to add a compiled-backend optimization, with a test harness that proves
it parity-safe, AND both headline passes show a measurable improvement.

Milestones 0–1 are low-risk and independently shippable. Milestone 2 is the
atomic/scary one (the recursive-core conversion) — but it ships *byte-identical
WAT*, so the blast radius is contained to "did the printer reproduce the
string," which the differential net answers immediately. Milestone 3 is
mechanical once 2 is green. Milestone 4 is where the project finally gets the
optimizable IR it wanted — and crucially, none of milestones 0–3 are *worth
doing on their own* (per §A's honest "modest value" assessment of binary
emission); **the justification for the whole effort is Milestone 4**, so 0–3
should only be undertaken with 4 as the committed goal.

---

## 7. Open questions / risks

1. **Effect ordering without an SSA store-graph.** WIR's value-tree must not let
   a pass reorder a `CallHost` (capability effect) past another, or hoist an
   effectful node out of a branch. v1 answer: mark nodes pure/effectful and
   forbid reordering across effectful nodes (conservative, simple). If that
   proves too coarse for code motion, the principled fix is **RVSDG-style state
   edges** — a reason RVSDG is noted as the future mid-end, not a v1 rejection.

2. **Where uniqueness ultimately lives (§3.1).** Analyze-then-lower is the v1
   call and keeps a single source of truth shared with the interpreter oracle.
   The risk is that inlining (Milestone 4) creates *new* accumulator shapes the
   AST-level analysis never saw, leaving optimization on the table. Re-running
   uniqueness on WIR recovers them but risks a second definition of "in-place"
   diverging from the oracle — gated behind the forced-copy differential if ever
   pursued.

3. **The atomic Milestone-2 conversion.** It is genuinely a red-tree-risk
   rewrite of the recursive core. The byte-identical-WAT discipline is the
   mitigation, but it demands the printer reproduce *every* whitespace/ordering
   quirk of the current emission, or the differential is noisy. Option: relax
   the assertion from byte-identical text to *semantically-identical assembled
   binary* (`wat::parse_str` of both, compare modules), trading exactness for a
   less brittle gate. Recommend starting byte-identical (catches the most) and
   relaxing only if churn demands.

4. **`EqShape`/`Render` helper explosion.** Per-shape helper synthesis already
   exists; WIR doesn't worsen it, but CSE/inlining could duplicate helpers if
   helper identity isn't interned across the module. Keep the existing
   memoization keyed by `EqShape::id()` when synthesizing `WirFunc`s.

5. **`Slot` typing at generic boundaries.** A monomorphized-but-generic value
   passes as `WirTy::Slot` (the i32/i64 universal ABI). Passes must treat `Slot`
   conservatively (no shape-directed rewrite). This mirrors today's
   "type-variable stays i32" rule (`ty_kind`), and is the existing
   monomorphization gap (architecture.md "Known gaps"), not a new WIR problem —
   but passes must be written to respect it or they'll mis-optimize a generic.

6. **Do we ever want true SSA?** If a future witchy optimization genuinely needs
   global value numbering or dominator-based code motion that the structured
   tree makes awkward, the escape hatch is a *local, per-function* SSA view
   (block-params à la Cranelift) computed on demand and discarded — not a change
   to WIR's structured substrate. Flagged so the structured choice isn't read as
   "SSA is forbidden forever," only "SSA-CFG is not the substrate."

---

## Appendix: why this is the right amount of IR

The brief asks for a *level* recommendation between "thin wasm-instruction IR
(cheap, barely more optimizable)" and "rich mid-level witchy-typed IR." The
recommendation is the **rich-but-pragmatic** middle: witchy-typed expression
nodes over a wasm-structured spine. A thin wasm-opcode IR would reproduce the
current problem (you still can't see `List(Int)` vs pointer, so the
slot-conversion and in-place wins stay out of reach); a full SSA-CFG mid-end
would buy a relooper and duplicate Cranelift. The chosen level is exactly high
enough to host the four optimizations only witchy can do, and exactly low enough
that lowering to wasm is a mechanical walk with no control-flow recovery — and
it *starts* where codegen already operates (a typed value-stack expression
tree), so it can be reached incrementally and grown pass-by-pass rather than
designed all at once.

---
rfc: 0142
title: "Structural program configuration and deterministic overload resolution"
status: proposed
created: 2026-08-21
superseded-by:
tracking: "Proposes structural record configuration and deterministic overload resolution rules for multi-tier API unification in Witchy and Glamour; no implementation is implied by this RFC."
related:
  - "0107 (glamour next-generation web framework)"
  - "0113 (full keyword arguments)"
  - "0125 (core language contract)"
  - "0136 (impregnable and ergonomic glamour)"
  - "0141 (canonical constraint solving and staged type checking)"
---

# RFC-0142: Structural program configuration and deterministic overload resolution

## Summary

Witchy's Model-View-Update (MVU) frontend framework, Glamour, currently provides three distinct constructor functions for state machine construction: `simple_program` for pure widgets, `command_program` for effectful state machines, and `program` for full applications with global subscriptions. While these distinct names provide explicit architectural clarity, they introduce API fragmentation and diverge from developer expectations established by languages with unified entry points (such as Python's `@overload`, TypeScript overloads, and Java method overloading).

This RFC proposes a unified, zero-cost configuration model that enables a single `glamour.program(...)` entry point. It specifies:

1. **Structural Configuration Dispatch**: Leveraging Witchy's structural records and width subtyping to accept open configuration records (`glamour.program(.{ ... })`) where omitted lifecycle fields automatically supply canonical zero-cost defaults.
2. **Deterministic Overload Resolution Hierarchy**: A formal, non-backtracking 3-stage resolution algorithm that eliminates ambiguity between function overloads and closure parameter types without sacrificing Hindley-Milner type inference.
3. **Monomorphic WebAssembly Lowering**: Ensuring that structural configuration records and overload dispatches are entirely resolved and erased at compile time, emitting direct monomorphic function instantiations without vtables or dynamic heap allocation.

## Motivation

In Glamour ([RFC-0107](0107-glamour-next-generation-web-framework.md), [RFC-0136](0136-impregnable-and-ergonomic-glamour.md)), the full application lifecycle requires six functions:

```witchy
glamour.program(
    authorize:     fn(UiRoot) -> auth,
    initial:       fn(Start) -> model,
    start:         fn(auth, model) -> Cmd(msg),
    update:        fn(auth, model, msg) -> (model, Cmd(msg)),
    view:          fn(model) -> Ui(msg),
    subscriptions: fn(auth, model) -> Sub(msg),
)
```

For simple widgets (such as counters, calculators, or read-only interactive visualizers), the author possesses no host capabilities (`auth = Nil`), issues no commands (`NoCmd`), and listens to no global subscriptions (`NoSub`). Passing five dummy closures is unnecessary ceremony. To solve this, RFC-0136 introduced `glamour.simple_program` and `glamour.command_program`.

However, maintaining three parallel constructors introduces friction:

- **API Discovery**: Developers must recall and select between three different constructor names (`simple_program`, `command_program`, `program`) rather than learning a single unified concept.
- **Refactoring Churn**: Upgrading a pure widget to issue an asynchronous HTTP command requires renaming the constructor and updating its call site rather than simply adding a command to `update`.
- **Closure Type Ambiguity**: Simply overloading `glamour.program` via ad-hoc function signatures creates a cyclic dependency during bidirectional type checking: the compiler cannot infer the types of untyped closure parameters (`fn(m, msg): ...`) without knowing which overload was selected, but cannot select the overload without knowing the closure's signature.

Witchy requires a principled approach that reconciles the convenience of unified call sites with the predictability of static Hindley-Milner type inference.

## Guide-level Explanation

### 1. Unified Program Creation via Structural Records

Authors define their application using `glamour.program` with an anonymous structural record:

```witchy
import glamour
from glamour import Ui

type Model:
    count: Int

type Msg:
    Inc
    Dec

fn update(m: Model, msg: Msg) -> Model:
    match msg:
        Inc -> Model(m.count + 1)
        Dec -> Model(m.count - 1)

fn view(m: Model) -> Ui(Msg):
    glamour.ui(glamour.element("div", [], [
        glamour.element("button", [glamour.on("click", Inc)], [glamour.text("+")]),
        glamour.element("span", [], [glamour.text("Count: ${m.count}")])
    ]))

fn main(console: Console):
    // Pure state machine (defaults: auth = Nil, start = NoCmd, subscriptions = NoSub)
    let app = glamour.program(.{
        model: Model(0),
        update: update,
        view: view,
    })
```

When asynchronous effects or capability-gated tasks are needed, the author provides an `authorize` hook and an effectful `update`:

```witchy
let app = glamour.program(.{
    authorize: fn(root: UiRoot): MyAuth.new(root),
    model: Model.empty(),
    update: fn(auth: MyAuth, m: Model, msg: Msg) -> (Model, Cmd(Msg)):
        match msg:
            FetchData -> (m, http.get(auth.fetch, "/api/data", DataReceived))
            DataReceived(data) -> (Model.loaded(data), NoCmd),
    view: view,
})
```

### 2. Supported Configuration Shapes

Glamour recognizes three canonical structural configurations:

1. **Pure Configuration**:
   - Required: `model: M`, `update: fn(M, Msg) -> M`, `view: fn(M) -> Ui(Msg)`
   - Defaults: `authorize = fn(_: UiRoot): Nil`, `start = fn(_: Nil, _: M): NoCmd`, `subscriptions = fn(_: Nil, _: M): NoSub`
2. **Command Configuration**:
   - Required: `authorize: fn(UiRoot) -> Auth`, `model: M`, `update: fn(Auth, M, Msg) -> (M, Cmd(Msg))`, `view: fn(M) -> Ui(Msg)`
   - Defaults: `start = fn(_: Auth, _: M): NoCmd`, `subscriptions = fn(_: Auth, _: M): NoSub`
3. **Full Lifecycle Configuration**:
   - Required: `authorize: fn(UiRoot) -> Auth`, `initial: fn(Start) -> M`, `start: fn(Auth, M) -> Cmd(Msg)`, `update: fn(Auth, M, Msg) -> (M, Cmd(Msg))`, `view: fn(M) -> Ui(Msg)`, `subscriptions: fn(Auth, M) -> Sub(Msg)`

## Reference-level Explanation

### 1. Deterministic Overload Resolution Hierarchy

To avoid infinite search or ambiguous constraint satisfaction during bidirectional type checking, overload resolution proceeds through three strictly ordered phases:

```
[Call Site] 
    │
    ▼
Phase 1: Key Presence Discrimination (Structural Record / Named Args)
    │── Matched unique key set ──> Select Target Candidate
    │── Indeterminate / Positional ──> Proceed to Phase 2
    ▼
Phase 2: Parameter Arity Discrimination
    │── Matched unique arity ──> Select Target Candidate
    │── Identical arity candidates ──> Proceed to Phase 3
    ▼
Phase 3: Parameter Type Constraint Propagation
    │── Unambiguous type match ──> Select Target Candidate
    │── Ambiguous / Insufficient Type Info ──> Emit Compile-Time Error (Require Ascription)
```

#### Phase 1: Key Presence Discrimination

When a call uses keyword arguments or a structural record literal, the set of provided key names is matched against the candidate signatures:

- If the key set contains `authorize` and `update` (with 3-arg signature), Candidate 2 or 3 is selected.
- If the key set contains `model` and lacks `authorize`, Candidate 1 (Pure) is selected.
- If an extraneous or misspelled key is present, the compiler rejects the call with a diagnostic listing the closest valid configuration shapes.

#### Phase 2: Parameter Arity Discrimination

For direct positional calls (`glamour.program(m, update, view)` vs `glamour.program(auth, init, start, update, view, sub)`):

- 3 positional arguments map uniquely to Candidate 1 (`model`, `update`, `view`).
- 4 positional arguments map uniquely to Candidate 2 (`authorize`, `model`, `update`, `view`).
- 6 positional arguments map uniquely to Candidate 3 (`authorize`, `initial`, `start`, `update`, `view`, `subscriptions`).

Because the arities (3, 4, and 6) are strictly disjoint, positional resolution is fully deterministic without inspecting closure types.

#### Phase 3: Closure Ascription Rule

If two overloaded functions share both identical argument arity and identical argument labels, but differ in the types of their parameter callbacks, the compiler applies the **Closure Ascription Rule**:

- If an untyped lambda (`fn(a, b): ...`) is supplied to a position where candidate parameter types differ, type checking cannot guess the user's intent.
- The compiler emits a clear, actionable diagnostic:
  ```text
  error[E0421]: ambiguous overload resolution for `glamour.program`
    --> src/main.witchy:12:13
     |
  12 |     let app = glamour.program(model, fn(a, b): ..., view)
     |               ^^^^^^^^^^^^^^^ ambiguous candidate signatures
     |
     = note: candidate 1 expects `update: fn(Model, Msg) -> Model`
     = note: candidate 2 expects `update: fn(Auth, Model) -> (Model, Cmd(Msg))`
     = help: add explicit type annotations to closure parameters: `fn(a: Model, b: Msg)`
  ```

### 2. Compilation and Wasm Lowering

Structural configuration records do not exist at runtime:

1. **Compile-Time Expansion**: During the lowering pass (`witchy-lower`), `glamour.program(.{ ... })` is rewritten to the canonical concrete `glamour.program` constructor call with default functions spliced in place.
2. **Closure Inlining**: Default closures (`fn(_: UiRoot): Nil`, `fn(_: Nil, _: M): NoCmd`) are marked `comptime inline` and eliminated during Wasm generation.
3. **Monomorphic Types**: No dynamic dispatch, vtables, or existential boxes are generated. The output WebAssembly binary contains identical machine code to an application written with handwritten monomorphic constructors.

## Drawbacks

1. **Language Surface Complexity**: Adding structural configuration patterns requires documentation and diagnostic support in compiler error formatters.
2. **Error Diagnostic Overhead**: Incomplete structural records require descriptive error messages that explain which required keys were missing from the configuration.

## Rationale and Alternatives

### Alternative 1: Retain Separate Named Constructors Only
Keep `simple_program`, `command_program`, and `program` as the only API. While conceptually simple, it creates API fragmentation and increases refactoring friction when migrating across lifecycle tiers.

### Alternative 2: Full Ad-hoc Java/C++ Method Overloading
Allow arbitrary functions with identical names across all scopes. This breaks Hindley-Milner type inference for first-class function values and leads to combinatorial explosion during constraint solving.

### Alternative 3: Builder Pattern Objects
Provide a mutable builder (`GlamourApp.new().with_model(...).with_update(...)`). This introduces unnecessary object lifecycle states and conflicts with Witchy's pure value semantics and capability discipline.

## Unresolved Questions

1. **Syntax for Record Defaults**: Should Witchy support general-purpose default values in nominal record declarations (`type Config: count: Int = 0`), or should defaults remain confined to function parameters and standard library constructor macros?
2. **Diagnostic Elaboration**: How should compiler errors format suggestions when 4 out of 6 required fields of a complex configuration record are supplied?

## Future Possibilities

1. **Extensible Component Island Plans**: Using structural records to allow custom island hydration triggers (`activate: .OnVisible`, `activate: .OnInteraction`) without expanding constructor signatures.
2. **Generalized Configuration Traits**: Allowing third-party libraries to define custom structural configuration converters using a standard `IntoProgram` trait once trait constraint normalization ([RFC-0141](0141-canonical-constraint-solving.md)) lands.

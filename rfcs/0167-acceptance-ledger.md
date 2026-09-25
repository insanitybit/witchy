# RFC-0167 implementation and adoption ledger

Baseline: `418a69b9ca75ae70fae5e96f797057e161f93aeb` (2026-09-16).
The RFC is the accepted target design, not an implementation closeout.
All rows below are open until their exact current production paths are tested.
Existing tests and historical reports remain useful evidence inputs, not blanket
proof of this new contract. Optimization expansion is outside this cut;
preserving the facts and representation freedom it requires is inside it.

## Dependency graph

```text
review/adopt contract
  -> syntax + migration + tooling
  -> ownership/view/callable type contracts
       -> optimization-readiness witnesses + fact-preserving summaries/IR
       -> normal/opt entry ABI -> canonical lowering + runtime
       -> validated canonical SafeWIR
            -> canonical SafeWIR interpreter oracle
            -> admitted unoptimized Wasm production runtime
  -> independent expected-result, diagnostic, effect, and structural matrices
  -> freeze source tree walker
       -> preserve comptime/build temporarily
       -> extract linker/tooling support
       -> migrate playground to compiled Wasm
  -> installed workflows + current spec/book -> implementation closeout
```

Syntax/tooling, test specification, and frontend contract work can proceed in
parallel once the contract is adopted. Backend tracks share a frozen call
envelope: owned result, ordered `var` outputs, immutable view root/projection
carriers, affine state, source mode, and authenticated authority. No optimization
is needed for soundness.

| ID | Contract and necessary evidence | Baseline gap / state |
|---|---|---|
| A | Review and adopt the exact RFC choices; record unresolved objections before changing the compiler | **PROVEN for the revised target decision only:** the 2026-09-18 memory-model review led to an explicit contract revision. The language keeps immutable `&T`/`&[T]` rooted views, removes `View(T, 'a)`, lifetimes, and `&mut`, and reserves mutation/write-back for `var`. This does not prove implementation; rows B, G, H, N, and O were reopened against the revised contract. |
| B | Canonical parser/formatter/LSP/Tree-sitter/reflection agree; legacy borrow forms only in migration grammar | **PROVEN for the canonical source and editor surfaces (2026-09-24):** ordinary parsing and checking use `&T`, `&[T]`, and `&place` in both modes and reject explicit lifetimes, `View(T, 'a)`, `&mut`, and write-through assignment. Historical RFC-0122 integration and example corpora that asserted the retired lifetime, mutable-reference, dereference-write, loan-telemetry, and source-interpreter parity model have been deleted rather than migrated into false semantics; one four-spelling canonical parser census preserves the negative contract. The executable immutable rooted-view corpus remains under RFC-0167. Historical fixtures that still test retained ownership behavior use an explicit migration-only parser; canonical formatting refuses its retired mutable-reference nodes. Structured comptime reflection retains the slice element argument while `meta.type_source` now reconstructs the single canonical `&[a]` spelling instead of the invalid duplicated `&[a](a)` form. The exact lead type-library census is 471 passed, 0 failed after replacing or deleting only superseded reference assertions. The remaining surface evidence is executable: `format_tests::rfc0167_formatter_emits_only_lifetime_free_rooted_views` and `rfc0167_formatter_preserves_bracketed_function_type_params` round-trip canonical views and refuse migration-only mutable references; `reflect::tests::rfc0167_reflection_preserves_lifetime_free_rooted_view_access`, `rfc0167_reflection_uses_a_lifetime_free_rooted_view_descriptor`, and `rfc0167_reflection_keeps_nested_aggregate_views_structural` preserve their exact types; `lsp_tests::rfc0167_lsp_reference_signatures_are_canonical` checks diagnostics, hover, and completion in both modes while `rfc0167_lsp_rejects_retired_syntax_instead_of_migrating_it` checks refusals; and `editors/tree-sitter-witchy/test/references.mjs` checks canonical and retired forms in both modes plus highlight queries. A 2026-09-24 source scan finds no retired `View(...)`, `&mut`, explicit reference lifetime, or `Nil` spellings in `std/`, `examples/`, `projects/`, or `book/src/`; `std/meta.witchy` uses structural `TView` and emits canonical `&` source. This closes syntax and tooling parity, not the separate row M typed-generation boundary. |
| C | `()`/omitted-unit signatures, inferred private returns, layout boundaries, explicit `let _ =` discard; negative old spellings | **PROVEN for the canonical source and installed workflow (2026-09-24):** source `Nil` was migrated to `()` and ordinary parsing rejects it; `()` now remains a structural empty tuple through parsing and checked products instead of recreating `Nil`. Focused witnesses give explicit and omitted public free/method/generic unit callables zero canonical result lanes, preserve the inferred private `Int` lane, and require annotations for inferred non-unit public free functions and methods. That unit-callable matrix now matches its independent `effect`, `41` expectation in the canonical SafeWIR interpreter and admitted unoptimized Wasm with zero leaks; the source interpreter is absent. Generic `own a` specialized to `()` now uses a value-owned Unit parameter with no result lane, and `()` closure captures use the same scalar Unit layout; initial and final SafeWIR witness checks bind both representations to checked source. The non-unit `var`-call exemption is removed: resolved statement calls now require unit result or explicit `let _ =`, and known intentional mutator-result discards have migrated. **2026-09-21, three of those four legs are now discharged.** EFFECTS/`must`: `tests/misc/rfc0167_unit_discard_matrix.rs` pins that a unit result stands alone as a statement in both spellings (omitted and `-> ()`), that a non-unit result is refused with a message naming `let _ =`, and - the part the row was really asking - that `let _ =` discards a VALUE but does NOT discharge an OBLIGATION: `let _ = chan.spawn(..).await` is refused as a must-consume drop. The positive program runs on the canonical chain with zero leaks. GENERATED-SOURCE: derives compile and print through the same discard rule, and the whole-repository corpus is compiled by `rfc0087_migration_census`, which went green on 2026-09-21 after two book blocks migrated to `let _ =`; that census IS the BROAD-CORPUS leg. Remaining: the installed-workflow leg, which is row O's `check`/`fmt`/compile/run surface rather than a separate matrix here. **Installed-workflow leg, 2026-09-22:** `tests/misc/rfc0167_installed_workflow.rs` drives the exact-tip `witchy` binary (`CARGO_BIN_EXE_witchy`): `check` accepts a contract program with omitted and written unit results and `let _ =`, the run prints its independent transcript, and `fmt` leaves it unchanged; `check` names `Nil` as retired. With this the fourth leg is discharged; the executable checks cited here establish the complete row C contract. **Line-leading minus, 2026-09-23:** a line beginning with `-` continued the previous line as a subtraction, so `let a = 5` followed by `-a` parsed as `let a = 5 - a` ("unbound variable `a`"), and with a bound name silently computed a different value. A line-leading `-` now begins a new expression under the same rule as line-leading `&`/`*` (which also subsumes the old match-arm-only negative-pattern case); a trailing `-` still continues. No std, example, project, or book source relied on the old reading. Pinned by `parser_tests::a_line_leading_minus_starts_a_new_expression` and `rfc0167_try_context_and_leading_minus::a_line_leading_minus_is_a_negation` (independent -5072, admitted unoptimized Wasm, zero leaks). |
| D | `own` consumes in both modes, including view carriers; missing result annotation cannot change consumption | **PROVEN for the admitted carrier (2026-09-21):** the revised evidence is executable in `tests/misc/rfc0167_own_view_consumption.rs`, on the RFC-0167 chain only (independent scalar expectation, canonical SafeWIR interpreter, admitted unoptimized Wasm, zero-leak audit on both; the source interpreter is not consulted). `own value: &Int` is an affine hold on the CARRIER binding: the root stays readable after the view is consumed, a second view of the same root is ordinary, and both hold through an indirect call and through a private return whose view outlives the call. Naming the consumed carrier again is refused. The annotation clause is proven by comparison rather than by assertion: the same program with and without the result annotation produces the SAME refusal text, so a missing annotation cannot change what the call consumes. Earlier evidence stands: exact normal/opt ordinary-value consumption, closure snapshot isolation, and the ordinary `own String` case matching its independent expectation with zero leaks. SCOPE: rooted views admit the canonical `Int` carrier, which is row G's recorded boundary. |
| E | Unique root versus nested proofs; normal copy repair; opt rejection; no duplicate unique result plus write-back | **PARTIAL (2026-09-24 live-view regression):** landed direct normal-to-opt list write-back preserves retained snapshots and SafeWIR uniqueness, while opt rejects a missing proof. Focused authenticated function-value and normal-relay witnesses preserve unique write-back through indirect canonical calls, and the row matrix pins the opt missing-proof source diagnostic. Root `unique List(Int)` has the ordinary physical list layout at local and `own` ABI boundaries; an exact `own unique` parameter may transfer that root-owner proof directly to the result. The source proof follows completing branch and early-return paths and recursively checks declared nested owners. One shared return-proof traversal covers exact `Result(unique T, E)`: `Ok` must establish a fresh/direct/`own` root, `Err` carries no success root, and exact direct Result relays preserve the contract. The seven-case matrix rejects direct, early, nested-Result, and function-value attempts to duplicate a `var unique` root while admitting a distinct fresh result across normal-to-opt-to-opt source boundaries. A direct `var unique List(Int) -> unique List(Int)` lowers the declared result and write-back as two distinct authenticated unique lanes; focused intra-opt and normal-to-opt-to-opt witnesses produce the independent expected value in the canonical SafeWIR interpreter and admitted Wasm with zero leaks. The bounded `Result(unique List|Dict, E)` carrier retains its exact checked signature, authenticates one unique owned wrapper graph plus the independent write-back lane, executes both `Ok` and `Err` paths with the independent expected value in the canonical interpreter and admitted Wasm with zero leaks, and rejects nested payload qualifiers, qualified error branches, and arbitrary aggregate roots. **Live-view opt guard (2026-09-24):** after forming `&xs[0]`, `mode opt` now refuses direct and function-value `var unique List(Int)` calls with a hidden-copy diagnostic. The normal-mode witness retains the old view and writes back the new root (hand-computed `1:9`, admitted unoptimized Wasm, zero leaks); the opt no-view witness computes `9` on the same admitted path with zero leaks. A discarded non-tail direct `&name` view no longer leaves a spurious opt mark; the existing canonical SafeWIR and admitted Wasm witness computes the hand result 9 with zero leaks, while earlier live marks and projected views still reject. **Open:** the guard remains monotonic for other expressions and does not prove alias transfer, rebinding, shadowing, or general liveness; nested uniqueness and the full row remain partial. |
| F | Frozen move and whole replacement work; deep mutation rejects; qualifier/view combinations follow one matrix | **PROVEN for the admitted carrier (2026-09-21):** the revised evidence the row asked for is now executable in `tests/misc/rfc0167_frozen_view_matrix.rs`, on the RFC-0167 chain only (independent scalar expectation, canonical SafeWIR interpreter, admitted unoptimized Wasm, zero-leak audit on both; the source interpreter is not consulted). `&frozen Int` keeps its payload contract as a parameter, a nominal aggregate field, a generic, and an indirect call through a function value. A view keeps its snapshot after the root is reassigned, which is the copy-on-write half of the contract. `frozen &T` and `unique &T` are refused as outer qualifiers with the diagnostic that names the target position; a write-through deref is refused as retired; a frozen payload refuses in-place mutation and is replaced whole. SCOPE: rooted views admit the canonical `Int` carrier, which is row G's recorded boundary (it also records the `&Float` refusal), so widening the carrier family is row G's work, not a gap in this matrix. Earlier frozen evidence in this row stands: frozen list and nominal-aggregate whole replacement, generic frozen transfer, and indirect frozen replacement match independent expectations in both modes, and deep mutation rejects before lowering. |

| G | Immutable rooted views through returns, aggregates, generics, traits, callables, indirect calls, loops, projections, and errors | PARTIAL: checked facts and canonical lowering carry authenticated root/projection metadata, and exact direct, generic, local function-value, and trait-mediated `&Int -> &Int` programs match an independent result of 79 in the canonical interpreter and admitted unoptimized Wasm with zero leaks. Callable metadata binds a normalized payload identity plus the exact carrier family, stays closed-local, uses distinct encoding tags from anonymous unions, and rejects unadmitted `&Float` rather than erasing it. The retained callable fixture now uses only this immutable carrier and canonical triad; its retired mutable/lifetime cases were deleted. A nongeneric nominal `Holder` whose nonempty fields are direct `&Int` now stores, relays, and reads the authenticated carrier, declares the required `Retain`, matches independent result 79 in both canonical engines with zero leaks, and still rejects region-born roots. Concrete `Option(&Int)` now preserves its exact nested view identity through layout, lowering, owner-family authority, `Some`/`None`, matching, calls, and returns; its independent result 790 agrees with the canonical interpreter and production `spawn_admitted` execution with zero leaks, erased owner identity rejects, and `var Option(&Int)` remains fail-closed instead of silently losing write-back. Shallow non-packed nominal, tuple, and `Result(&Int, String)` carriers now restore their exact source-derived opaque owner ABI instead of accepting the checked scalar erasure: a two-field nominal plus tuple program produces the independent result 494 and the `Result` program produces 7 in the canonical interpreter and admitted Wasm with zero leaks, while both region-root escapes reject. Nested aggregate views and non-`Int` payload families remain fail-closed. Frontend `&[a]` preserves element/range projections and exposes only `.length()`; by-value `.at()` and all mutators reject, including unique/must near-misses. An exact returned `&[Int]` projection now transports its authenticated `{root,start,len}` carrier through a callable, produces the independent projected length 2 in the canonical interpreter and admitted unoptimized Wasm, and releases the retained root with zero leaks; the same boundary refuses `&[Float]` before execution. Generic `first[a](&[a]) -> Option(&a)` now specializes to the exact `Option(&Int)` contract and composes `&items[0]` as an authenticated nested projection from the retained slice carrier. Its independent Some result 20 and None result 1 agree in the canonical interpreter and admitted unoptimized Wasm with zero leaks; the reader checks local index against captured slice length, checked-adds the slice start, rechecks the absolute index against the live backing-root length, and the region-born-root negative rejects. Broader trait payloads, aggregate closure capture, nested/broader aggregates, loops/errors, and broader retain/release audits remain open. Existing nominal-lifetime and exclusive-reference matrices are superseded and do not count. **Carrier-widening cost, measured 2026-09-21 (attempted and reverted, so the next attempt is scoped):** the ABI half is small - `CanonicalScalarKind` already has `Float`, `rooted_scalar_payload_key` is already parameterised by kind, `CanonicalRootedViewCarrier` has two variants with only TWO production construction sites, and adding a `FloatScalarRoot` alongside them gets `&Float` through the ABI. The LOWERING half is the real cost: a rooted view is carried by a dedicated owner family built for `&Int` in `canonical_lower.rs` (`shared_reference_owner` with a hardcoded `Type::Named("Int")` source, its own `GcTypeId`, list type, signature, environment layout and constructors, mirrored by a second family for the `Slice(Int)` root). A second scalar payload needs its own family, so widening is a feature with layout and constructor work, not an enum variant. `UnsupportedType(Qualified(View, Named("Float")))` is where the attempt stops. **Nested `Option` carriers, 2026-09-21:** `Option` around a view carrier now nests to any depth, with each shell its own authenticated opaque owner whose `Some` field is the next carrier inward. The owner identity (`canonical_owner_aggregate_identity`), the lowering and authority carrier predicates, the checker ownership tree (one `LayoutDependent` node per shell over the `RootedViewCarrier` leaf), and refutable nested patterns over an opaque owner (`Some(Some(v))`) all follow that recursion. A record holding a direct `&Int` beside an `Option(&Int)` is carried field-by-field instead of being refused; the previous negative that pinned the refusal is replaced by its positive witness. `Result` or tuple shells around an `Option` view, `&String`, and `List(&Int)` remain fail-closed. **General nested carriers, 2026-09-21:** the carrier judgment is now one recursive predicate, `canonical_aggregates::is_rooted_int_view_carrier`, shared by layout identity, lowering, and authority. A carrier is the direct `&Int`, or a non-packed tuple, record, user sum, `Option`, or `Result` in which every field is either a carrier or holds no view, with at least one carrier field. Every shell is its own opaque owner, so `Option((&Int, Int))`, `Option(Result(&Int, String))`, `(Option(&Int), Int)`, `Result(Option(&Int), String)`, a record inside a record, and `Full(&Int)` all admit. This replaced two duplicated shallow `contains_rooted_view_descendant` walks. A field holding a view outside that shape (`List(&Int)`, `&String`, `&[Int]` inside an aggregate) still keeps the whole aggregate fail-closed. **Shells around records, 2026-09-21:** `Option(Inner)`, where `Inner` is a record carrying a view, failed with an internal MissingCheckedReferenceRelation. The checker's ownership classification is structural and leaves nominal layouts opaque, so a shell whose only views sit inside a nominal has no ownership output to compare. Such shells now use the same opaque-owner family binding that already covers a bare nominal result. A structurally visible view still requires the exact ownership tree. Witness `an_option_of_a_view_carrying_record_is_a_carrier` gives 102 on both engines with zero leaks, and the region escape through `Option(Inner)` still rejects. Root provenance across nominal records rests on that family binding plus typeck's region-escape judgment (SEC-096); the authority layer does not re-derive which parameter a nested nominal view came from, and none is needed for soundness because a view retains its own root. 2026-09-22: whole `&String` views lower through the retained-root `StringRoot` carrier (param, return, local, deref read, generic `&a`); closure capture and aggregates holding `&String` stay fail-closed; `&Float` still refused. **Field views, 2026-09-22:** `&p.y` of an `Int` field (also nested `&o.inner.y` and tuple `&pair.1`) was refused as "rooted view projection is outside the canonical v1 carrier". It now forms the same authenticated `&Int` carrier as `&x` over the field value at formation, after checking that the source chain names the checked field steps from the checked root. Records are values, so this snapshot is exactly what the view observes: `rfc0167_field_view` gives 43, 62 across a call, 495 with the snapshot surviving reassignment of a `var` record, and 8 for a tuple element, on both engines with zero leaks. Non-`Int` field payloads remain fail-closed with the other non-`Int` carriers. Views through plain type parameters, 2026-09-22:** `fn id[a](x: a) -> a` called with `&n` type-checked generically, but monomorphization bound `a = Int`: `bind_ast_type_vars` stripped every qualifier, the view included, so the specialization rejected its own `&Int` argument ("expected `Int`, found `&Int`"). Binding now strips only ownership qualifiers, and matches a `&p` pattern against a `&c` argument structurally, so `id[a](x: a)` binds `a = &Int` while `keep[a](x: &a)` still binds `a = Int`. `rfc0167_generic_view_instantiation` gives 5 (identity), 75 (choose one of two views), 57 (`Option(a)` payload), and 9 (the `&a` pattern) on both engines with zero leaks. `List(&Int)` remains a separate, fail-closed carrier. **By-value slice element reads, 2026-09-22:** `xs[i]` on a `&[Int]` view failed as "opaque list index family", so a search over a slice could form `&xs[i]` but not compare `xs[i]`. The by-value read now lowers as the read through the checked element view (`*(&xs[i])`), which bounds-checks `i` against the slice's own length; any `&[Int]` carrier now also registers the shared `&Int` family its element views need. `rfc0167_slice_index_read` gives 5 (search) and 900 (sum over `&xs[1..4]` plus an empty slice), and an index that exists in the backing list but not in a one-element slice aborts in the canonical interpreter and traps in admitted Wasm. Captured `&String`, 2026-09-22:** a closure capturing a `&String` was refused in typeck ("only the canonical scalar carrier crosses a callable boundary") and then by the capture-environment check, which paired a `RootedViewByValue` capture only with owner families. A `&String` view is its retained String token, so its capture is a managed reference slot; typeck admits `String` view payloads for capture and the authority check accepts that slot only when the checked capture type is exactly `&String`. Witnesses give 15 and 37 (the snapshot survives the owner growing, read through an indirect call) on both engines with zero leaks. **Whole-record views, 2026-09-22:** `&p` for a user record or sum was refused ("rooted view payload is outside the canonical Int carrier" locally, `UnsupportedType(&Point)` across a call). It now uses a `RecordRoot` carrier: like `StringRoot`, the view is the retained record object, because a record is an immutable value shared by retain. The ABI payload identity is the declaration's nominal ordinal, shared by the checked-type and declaration encodings. Only declared, non-packed, argument-less records and sums are admitted; the lowering additionally requires a heap `Ref` root. `rfc0167_record_view` gives 34 (relay then field reads), 3479 (snapshot survives reassignment), 625 (a sum matched through `*`), and 34 (a record with a String field through a generic), on both engines with zero leaks, and a region-born record still cannot escape through its view. **String and record field views, 2026-09-22:** `&n.label` (a String field) and `&o.inner` (a record field) were refused as whole-root-only carriers. A view of such a field is now the field's retained token, from the ordinary field read after the same checked-field-chain validation the `Int` field views use (now one shared helper). Records are values, so the field read at formation is exactly what the view observes. Witnesses give 42, and 3412, where a record-field view taken across a call and a String-field view both keep their values after the owning record is reassigned; zero leaks on both engines. **Aggregates of `&String`, 2026-09-22:** a record field worked locally, but `(&String, Int)` and `Option(&String)` failed the callable signature correspondence (CandidateSignatureMismatch). A `&String` view is its retained String token, so inside an aggregate it has exactly the String layout: the authority check now reads nested `&String` as `String` when matching a candidate signature, while a callable's direct `&String` lanes keep their whole-root path. Witnesses give 52 (record field), 333 (tuple and `Option` across calls), and 347 (a record's `&String` field keeps its snapshot after the owner grows) on both engines with zero leaks; a region-born String still cannot escape inside an `Option` view. **Views of generic records, `Option`, and `Result`, 2026-09-22:** `&Box(Int)`, `&Option(Int)`, and `&Result(Int, String)` were refused; `RecordRoot` admitted only argument-less records. The record-root payload identity now also covers type arguments: a declared user type (its nominal ordinal) or builtin `Option`/`Result`, applied to scalars, `String`, or further such heads, written the same way from a checked type and from a declaration. Lowering and authority share `canonical_aggregates::is_record_root_payload`, which requires a non-packed heap shape with no view inside (`Option(&Int)` keeps its own owner carrier). Witnesses give 74 (`&Box(Int)` and `&Box(String)` in one program), 9340 (`Option` and both `Result` arms), and 38 (snapshot), on both engines with zero leaks. **One record-root rule at the ABI, 2026-09-22:** the canonical ABI now admits a `RecordRoot` view only when `canonical_aggregates::is_record_root_payload` holds, the same rule lowering and authority use, so a packed record or a record holding a view no longer gets a record-root key that later stages must refuse. A packed-record view is refused at the boundary (pinned by `a_packed_record_view_has_no_record_root_carrier`). Whole-list views, 2026-09-22:** `&List(T)` for scalar or String elements was refused. It now uses a `ListRoot` carrier, the retained list object; a later mutation of the owner finds the list shared and copies, so the view keeps its snapshot. `&List(Int)` never acquires the slice carrier's projection authority (distinct payload and carrier). `rfc0167_list_view` gives 3 (across a call), 616 (`push` after the view: old sum 6, new sum 16), 15 (`var` write-back grows the owner while the view keeps one element), and 5 (`&List(String)`), on both engines with zero leaks; a region-born list cannot escape through its view. **`&Float`, `&Bool`, `&Duration`, 2026-09-22:** these were refused as having no carrier. A `ScalarValue` carrier now carries each as the scalar value itself: an immutable scalar read at formation is exactly what its view observes (the RFC's proven-equivalent representation), and `(*v)` is the value. `&Int` keeps its `IntScalarRoot` cell, which also carries list element views. `rfc0167_scalar_value_view` gives 1 (`&Float` across a call), 10 (a `&Bool` snapshot survives reassignment), and 111 (Float, Bool, and Duration field views, one through a generic) on both engines with zero leaks. The fail-closed pins that used `&Float` now use `&List(List(List(Int)))`, which still has no carrier; closure capture of scalar views other than `&Int` remains refused. **Scalar element views and capture, 2026-09-22:** `&xs[i]` over a `List(Float)`/`List(Bool)`/`List(Duration)` is the element read at formation, bounds-checked like any index read (110: the view survives a later store; an out-of-range element view aborts in the canonical interpreter and traps in admitted Wasm). A closure may capture `&Float`, `&Bool`, and `&Duration` views as copied scalar slots (42). The capture-refusal pins now use `&List(List(List(Int)))`. **Record and list view capture, 2026-09-22:** a closure capturing a record view failed the capture-environment check, which admitted only `&String` in a reference slot. Every whole-root view (`&String`, a record or sum, a scalar List) is its retained token, so the check now accepts all of them in a managed reference slot. Witness `a_closure_captures_record_and_list_views` gives 132 on both engines with zero leaks. **Generic slices, 2026-09-22:** `&[Float]`, `&[Bool]`, `&[Duration]`, and `&[String]` views failed with "rooted view payload has no admitted authenticated carrier". They now use the same zero-copy `ListSliceRoot` carrier as `&[Int]` (retained list root plus checked `{start, len}`), one owner family per element. By-value `s[i]` is bounds-checked against the slice's own length, offset by its start, and read from the retained root behind the root's live bounds check. A scalar element view `&s[i]` is the value, and a String element view is the retained String token. `rfc0167_generic_slice_view` gives 112 (Float: pass, return, length, element view), 392 (String), 211 (Bool and Duration), 2129 (slices of slices, static and dynamic endpoints), 121 (generic `first` at Float and String, including None), and 42112 (the snapshot survives a later push and indexed store to the owner `var` list), all on both engines with zero leaks. An index or sub-slice end past the projection but inside the backing list aborts in the canonical interpreter and traps in admitted Wasm, and at that point nested-list slices still failed closed. `for x in slice` is still refused by the type checker. **`for` over a slice, 2026-09-22:** `for x in s` on a `&[T]` view was a type error ("`for` expects a List"). It now iterates the view's elements by value: the loop counts `0..s.length()` and reads each element through the same checked projection as `s[i]`. The carrier is immutable, so it dominates the loop without becoming loop state. `rfc0167_for_over_slice` gives `Int` 5, `String` 41, `Float` with `break` 2, `Bool` with `continue` 2, generic `[a]` including an empty slice 330, and nested loops 25, on both engines with zero leaks. Found BUG-647: mutating a `var` List inside any loop while an element or slice view of it is live fails admission (InvalidOwnership). A whole-list view `&xs`, or the same mutation outside a loop, works. **`List(&Int)`, 2026-09-23:** a list of `&Int` views had no layout (UnsupportedListElement), the SafeWIR checker refused every owner-list operation on a shared-reference family, and signature correspondence refused a view nested in `List`. `List(&Int)` is now the `&Int` family's owner list: each element is one retained, authenticated place carrier. The checker admits the list operations for that family; `ReadTag` and `CloneEnvironment` stay refused. Signature correspondence admits a `List(&Int)` parameter or result and nothing wider. `rfc0167_list_of_int_views` builds, pushes, indexes, and iterates a list of views (724, 27), keeps element snapshots after the root is written and grown (13100), crosses a parameter, a result, and a generic `List(a)` (3, 21, 2), replaces an element view without disturbing an earlier copy (121), leaves a loop over the list early (7), and still refuses a region-born view escaping in a list. The checker admits owner lists only for the `&Int` view family and checks that family's list id for uniqueness. Evidence is admitted Wasm with a zero-leak audit. Review found BUG-648 (a region-born view pushed through a `var` parameter passes typeck; only a lowering limitation stops it). **`?` on an `&Int` payload, 2026-09-23:** `?` on `Option(&Int)` or `Result(&Int, E)` failed canonical lowering with `OwnedReleasePlacement(MissingToken)`: an aggregate with an `&Int` payload is an opaque owner, but `?` read its tag and payload as plain record fields, so the payload had no token. `?` now reads tag, payload and error through the same source-aggregate readers as `match`, and rebuilds the failure for the caller's own (possibly opaque) result type. `rfc0167_try_int_view_payload` gives Option success 21, None propagation 5, Result Ok 60, Err propagation 12, `?` in a loop 10099, and `Result(&Int)` relayed into `Result(&Int)` 807, in admitted Wasm with zero leaks. **`.owned()`, 2026-09-23:** `.owned()` is compiler-known and needs no import; std `borrow` and its blanket `Owned` trait (which returned the view itself, typed `&T`) are deleted. The checker types it: `&T` materializes `T` and an owned value is identity, and it refuses authority, `must`, once-callable, view-bearing aggregates (`Holder` with a `&Int` field, `List(&Int)`), slices (the contract gives `[T]` no owned form), and a receiver whose type is still an inference variable. Trait lowering then emits exactly the view read `*view`, or the value itself; a type's own, a bounding trait's, or a `dyn` trait's `owned` method still wins. `rfc0167_owned_materialization` gives 450 (`&Int`), 513 (`&String`), 5435 (`&List(Int)`), 13473 (record), 23 (a view parameter returns a plain list), 37 (identity), 423422 (generic `&a` plus a user `owned` method), and 42 (a trait-bound `owned`). Mutating the root or the copy is invisible to the other. Evidence is admitted unoptimized Wasm with zero leaks; the canonical interpreter was not run. Row L region copy-out does not use it yet. **Views through views, 2026-09-23:** `&(*b).f` and `&(*v)[i]` were refused as non-places. The checked view place now admits one dereferenced view binding as its base (`CheckedViewFact::formed_through_view`); lowering authenticates the exact source chain and composes the projection on the view's retained root (field token, `ListIntElement` on the viewed list, or a composed `&[T]` slice), and `&r.xs[i]` / `&(*b).xs[i]` compose a field chain with a checked element step. Payloads without a carrier (e.g. `&(*v)[0]` for `List(Rec)`) still refuse. `rfc0167_views_through_views` gives field views through `&Rec` parameters 37580, list-view elements/slices/String elements 3302420, field-element views 762, and a formation-time bounds trap, all read after the root `var` is reassigned, in admitted Wasm with zero leaks. **Slice captures and view-bearing callables, 2026-09-23:** a closure could not capture a `&[T]` ("no admitted carrier"), and a callable whose signature carried a view (`fn(&String) -> &String`, `fn() -> Task(&Int)`) failed canonical lowering. A captured `&[T]` of an admitted element (Int, Float, Bool, Duration, String) is now retained into an owner field of its rooted-slice owner family. The callable prefilters in lowering and authority admit a direct view nested in a signature; the lanes are still bound by value correspondence, and a view with no carrier (`&List(List(List(Int)))`) stays refused. Aggregate identity, closure-family keys and the authority family key keep `&Int` and `&[T]` (each has its own opaque owner family) instead of erasing them to their payload. Before this, `fn() -> Step(&Int)` and `fn() -> Step(Int)` shared one family key with two physical carriers. `rfc0167_async_view_results` captures slices of all five elements and keeps each snapshot after the `var` root is written or grown: `22 100`, `32`, `4.0`, `false`, `1`, `abcde`. It calls `fn(&String) -> &String` indirectly (5) and still refuses a region-born slice escaping in a closure. Evidence is admitted Wasm with a zero-leak audit. Generic `.owned()` templates now specialize before checking each concrete receiver: an Int instance returns 7 in admitted unoptimized Wasm with zero leaks, while `&Int` and a nested view are refused with concrete diagnostics (`rfc0167_owned_materialization`). **Exact nominal slice field, 2026-09-23:** `tests/misc/rfc0167_slice_record_field.rs` proves a non-packed record can carry an exact `&[Int]` root and projection: the hand-computed result is 202 from `[1, 2, 3, 4][1..3]` (first element 2, length 2), matched by admitted unoptimized Wasm with zero heap leaks. A region-born root refuses escape through the field with a per-field parent-root provenance diagnostic; the owner-authority test refuses erased and wrong-family carriers. This closes only the direct `&[Int]` nominal-field leg; other slice payloads remain open. An indirect function-value call now transports `List(&Int)` with two retained element views: after the source list changes from `[3, 4]` to `[9, 4]`, the hand-computed result `(3 + 4) * 100 + 9 = 709` matches admitted unoptimized Wasm with zero leaks (`rfc0167_indirect_list_view`). The paired indirect `var List(&Int)` call refuses a region-born view at checking with the named write-back diagnostic. Broader nested payload carriers remain open. **Slice `.owned()` bounded materialization, 2026-09-24:** a checked `&[T]` projection with `T` exactly Int, Float, Bool, Duration, or String materializes a fresh `List(T)` containing only that projected range; the generic four-element witness has hand-computed result 11,120,603 in admitted unoptimized Wasm with a zero-leak audit, and a direct Int projection independently gives 12 with zero leaks (`rfc0167_slice_owned_materialization`, `rfc0167_owned_materialization`). The String extension is pinned by `rfc0167_slice_owned_materialization::string_slice_owned_retains_each_copied_element_and_the_original_view`: a view of `bbb` and `cccc` survives independent writes to its source and the materialized list, and the hand-computed `3*1000 + 2*100 + 1*10 + 4 = 3214` matches admitted unoptimized Wasm with `Some(0)` heap leaks. `complete_managed_abi_subrange_admits_bounded_clone` pins checker admission of a bounded non-prefix range from an authenticated complete managed ABI source. Nested List and must-bearing slice elements still refuse during source checking; other element families remain outside this admitted copy boundary, so row G remains PARTIAL. **Dictionary carrier boundary, 2026-09-24:** ordinary `Dict(String, String)` returns the hand-computed length 2 in admitted unoptimized Wasm with zero leaks. `Dict(String, &String)` fails canonical admission with the named missing collection carrier, both directly and as a populated nominal record field. A public `Holder -> Holder` relay whose field is `Dict(String, &String)` is also refused before an erased record ABI can cross a boundary (`rfc0167_dict_view_diagnostic`). This leaves dictionary view values unsupported pending an authenticated carrier; it does not close row G. **Exact whole dictionary roots, 2026-09-24:** `&Dict(String, Int)` retains its old one-entry root while its owner receives a second entry; `&Dict(String, Bool)` retains old `true` after its owner replaces it with `false`; `&Dict(String, Float)` retains old `1.5` after its owner stores `9.5`. The one existing `rfc0167_dict_root_view` program pins their hand-computed combined result 1112 on admitted unoptimized Wasm with zero leaks. The shared exact scalar DictRoot path now includes `Dict(String, Duration)` with a distinct source ABI key and Duration value-list layout. **IMPLEMENTED BUT UNVERIFIED:** its hand-computed snapshot, admitted unoptimized Wasm result, and zero-leak audit remain pending, so Duration does not close this row. Nested or qualified dictionary values still refuse at canonical admission, Bool and Float dictionary view closure captures refuse at type checking with the missing-carrier diagnostic, and region-born Int, Bool, and Float dictionary roots cannot escape. Whole-root admission does not grant dictionary collections of views or nested aggregate carriers. **`Option(&Float)` carrier, 2026-09-24:** `rfc0167_scalar_value_view::an_option_float_view_relays_without_admitting_other_payload_families` preserves a 2.5 snapshot after its owner changes to 9.5; the hand result 21 agrees with canonical SafeWIR and admitted unoptimized Wasm with zero leaks. An `Option(&Dict(String, Bool))` retains its exact `Qualified(View)` payload and fails canonical admission as `UnsupportedType`. **Exact slices of owned scalar lists, 2026-09-24:** the landed `rfc0167_slice_list_int_root_view` case gives the hand-computed 493 from an old inner-list snapshot after nested mutation, in admitted unoptimized Wasm with zero leaks; a qualified child is refused. The shared carrier structure now recognizes exact `&[List(S)]` for `S = Int`, `Float`, `Bool`, or `Duration`, assigns distinct root families, and requires an owned child pointer to a retained packed list with the exact inner scalar kind. Forged nominal labels, deeper lists, and qualified children fail closed. **IMPLEMENTED BUT UNVERIFIED:** the new Float, Bool, and Duration cases have not completed a focused compile, hand-computed admitted-Wasm run, or zero-leak audit at this tip; `.owned()` on exact `&[List(Int)]` has hand-computed admitted-Wasm and zero-leak evidence; `List(Float|Bool|Duration)` is implemented but unverified, and other pointer-element slices still refuse. Deeper aggregate and collection payloads remain open, so row G remains PARTIAL. |
| H | `var` is the sole mutation/write-back path; views preserve pre-mutation snapshots by copy-on-write | PARTIAL: ordinary parsing rejects `&mut`; immutable `*view` reads survive while assignment through `*view` rejects; canonical signatures preserve `var &T` as an opaque write-back lane rather than normalizing it to a shared parameter. BUG-635's control-flow write-back join, BUG-636's unit-returning reallocating `var List` mutator, and BUG-637's indexed mutator write-back are fixed on this branch. BUG-636's proof promotes only exact fresh allocations on every returning terminal and explicitly refuses indirect `VarTailCall`; its five focused canonical tests pass. Named callable adapters now rebind after body-derived fresh-write-back promotion, translate the hidden closure-environment parameter in both authority validators, and may forget a same-layout reallocation proof into `unique` only in the safe direction. The focused checker negative, adapter-structure witness, normal relay, and normal -> opt -> opt direct/indirect execution paths pass 4/4. Shared loop phis now rebase bounds/guard/fresh-header generations only when their scalar evidence crosses the edge, preserving authenticated bounds for an uncarried retained snapshot of the same physical root; the structural negative, exact three-leg list-pop production witness, and full uniform-`var` shard pass 1/1, 1/1, and 24/24. A structural list witness proves a later mutation allocates a distinct root while retaining the old view snapshot. Direct `var Holder(&Int)` replacement now preserves the prior field snapshot and produces the independent result 79 in the canonical interpreter and admitted Wasm with zero leaks. Indirect aggregate `var` callable transport and the broader normal/opt CoW matrix remain open. **Indirect aggregate `var`, 2026-09-22:** a `var` record or list written back through a function value (`fn(var Acc, Int) -> ()`) failed admission with CallableContractMismatch whenever the indirect call was in tail position. The callee's write-back result is `ReallocatesParameter`, the caller declares `Shared`, and the tail-forwarding rule admitted only the step from `ReallocatesParameter` to `Unique`. Fresh storage may be forwarded wherever a weaker claim of the same layout is declared, so `Shared` is now admitted too. `rfc0167_indirect_var_aggregate` gives record 72, list 31 with its pre-call snapshot intact, and record-holding-list 720 on both engines with zero leaks; the uniform-`var` shard still passes. **`var` List mutated in a loop (BUG-643), 2026-09-22:** a callee that pushed onto its `var` List parameter inside any loop was refused (IncompatibleMemoryJoin at the loop header): the entry token is the caller's shared write-back token and the back edge carries a fresh owned replacement. Every loop form now enters with an owned copy of a still-shared `var` List lane, as `var ys = xs` does. `bug643_var_list_loop` gives 560 (range `for`, `while`, and `for` over another list) and 1109 (the caller's earlier copy is unchanged) on both engines with zero leaks. **Projected views across a mutating loop (BUG-647), 2026-09-23:** a loop that mutated a `var` List while an element view, a slice view, or a closure over it was live failed admission (InvalidOwnership). A whole-list view `&xs`, or the same mutation outside a loop, worked. The owner `Construct` that forms such a view retains its root, but lowering recognized only `CloneRef` as sharing, so the loop claimed the List unique. Construct captures now count as shared, so the loop carries a shared lane and a push copies. `bug647_view_snapshot_in_loop` gives 6306 and 6303 (slice view under `while` and `for`), 6203 (element view with an index write), and 63 (closure) on both engines with zero leaks. **`var` view parameters, 2026-09-23:** `var v: &Int`, `var s: &[T]`, `var o: Option(&Int)` (and `var` callables) failed admission: the ABI authority and SafeWIR checker refused any GC-carried `var` parameter and lowering refused opaque owners. A GC carrier now takes one general write-back lane (WriteBack token in, same-type replacement carrier out, no `Write` effect: the viewed value is never mutated). Typeck refuses, inside `region:`, a `var` argument rooted outside the region whose type can hold a view or callable, closing BUG-648. `rfc0167_var_view_parameters` gives 2011, 26444, 292, 303009, 71 and 17 on admitted unoptimized Wasm with zero leaks, and pins both region refusals. 2026-09-23: normal modules may now name view-bearing records (link-time "reference-bearing opt type" policy narrowed to retired non-rooted carriers) and rebind a local `var` of one (expected 12/74, admitted Wasm, zero leaks); a `var` PARAMETER of a view-bearing record writes back through the GC write-back lane that `var` view parameters added (21 in `rfc0167_normal_view_bearing_types::a_normal_var_parameter_writes_back_a_view_bearing_record`, admitted Wasm, zero leaks). **Live-view call boundary (2026-09-24):** the opt source check now refuses a direct or function-value `var unique List(Int)` call after `&xs[0]`, because preserving that view would require a hidden copy; the normal witness produces the hand-computed `1:9` and the opt no-view witness `9` on admitted unoptimized Wasm with zero leaks. **Open:** precise view liveness and alias tracking across every carrier and call form remain to be proved, so this row stays partial. |
| I | Normal -> opt and opt -> opt direct/method/generic/trait/function-value calls; imported mode preserved; aliased snapshots unchanged | **PROVEN for the declared source-linked contract and authenticated external scalar ABI:** landed reference-free and direct `var unique List` normal-to-opt chains preserve mode metadata and snapshots on the canonical checked route. Immutable rooted `&Int` now crosses normal -> opt -> opt through qualified calls, `from` imports, and opt function values without type erasure; each form preserves the pre-mutation snapshot and matches the independent result 79 in the canonical interpreter and admitted unoptimized Wasm with zero leaks. An exact negative confirms the converse is not a missing carrier: RFC-0167's transitive opt contract rejects an opt module importing an arbitrary normal user module before canonical admission. A structurally linked but unadmitted `&List(Int)` boundary reaches canonical admission and rejects there rather than detaching or erasing the carrier; legacy/mutable and nominal reference carriers remain closed. Focused eta/function-value checks retain declared unique/frozen qualifiers and admitted indirect calls preserve unique write-back, including a normal relay, while rejecting a shared callable in that lane. The normal-to-opt-to-opt generic function-value plus trait-method witness retains both imported opt origins, lowers the generated trait method as opt, preserves unique provenance with every optional optimization disabled, and matches the independent snapshot result in the canonical SafeWIR interpreter and admitted canonical Wasm. Linked-source opt callable provenance survives return, alias, and nested higher-order application; normal callers may use opt leaf functions through their ordinary ABI, while normal callbacks, nested adapters, normal trampolines, nominal fields, and generic identities cannot satisfy or erase a stricter opt higher-order precondition. Nine local and seven separately parsed source-module cases cover direct imports/linker eta, returned aliases, nested higher-order calls, opt trait wrappers and dispatch, generic erasure rejection, and normal trampoline rejection. For separately compiled code, Coven/Grimoire materializes the exact signed Wasm and scalar export sidecar, the ordinary project resolver selects it from the exact lock, and the production `run-source` path launches it through `spawn_admitted_with_external_libraries`; the real CLI positive prints the independent expected value, while tampering refuses before launch. Recursive external callbacks and higher-order external ABI values are intentionally unsupported rather than silently erased. |
| J | Public and callable boundaries preserve nested view carriers and opt provenance without erasure | **PROVEN for admitted source-linked carriers (2026-09-23):** Executable in `tests/misc/rfc0167_view_carrier_transport.rs` on the RFC-0167 chain only: a view crosses a nominal record field, a type alias, a generic argument, AND a callable capture. Capture needed a compiler change: a blanket `typeck` guard refused any closure capture of an explicit reference because "a closure may escape its loan scope", reasoning in the lexical-loan model RFC-0167 retires in the same section that permits capture ("a view is a value-semantic snapshot handle, not a lexical loan ... may be returned, stored in an aggregate, transported through a generic, and captured by a callable without naming a lifetime"). The carrier retains the root while any copy is live, so capture is not an escape. The guard is NARROWED rather than deleted, because a capture must still not carry a payload the ABI has no carrier for: it now refuses on the payload, keeping the `&String` and `&List(Int)` refusals that `typeck::borrowed_nominal_cannot_escape_through_an_inferred_closure` and `pipeline::canonical_closure_witness_rejects_borrowed_capture_authentication` depend on. Five assertions that named the old loan-escape reason were updated to the carrier reason; their fixtures still refuse. **FIXED 2026-09-21.** The blocker was never opt mode. `Convention::Borrow` is the explicitly written `let`; `Convention::Let` is the default one. Both are non-owning reads of the same GC-carried view, but only `Let` appeared in the ABI's `(ValueType::Gc(_), ..)` ownership arm, so a `let value: &Int` parameter fell through to the `Value` arm while its contract said `Shared` and the signature was refused - in ORDINARY mode as much as in `mode opt`. Adding `Borrow` to that arm makes both modes admit, run, and match their independent expectation (`a_written_let_convention_carries_a_view_in_both_modes`). **PROVEN for the admitted carrier (2026-09-21).** The row's full carrier list is now executable in `tests/misc/rfc0167_view_carrier_transport.rs` on the RFC-0167 chain only: nominal record field, type alias, generic argument, callable capture, TRAIT method, and a CROSS-MODE bundle where an ordinary caller hands a view to an opt callee. One refusal that looks like a gap is correct and stays: a `let`-borrowed view parameter cannot be RETURNED, because §ownership makes `let` the nonescape contract ("Nonescape is expressed by `let`/`var` call contracts and compiler analysis") - returning the carrier is exactly the escape it promises not to make. SCOPE: the canonical `Int` carrier, which is row G's boundary. 2026-09-23: normal modules import (`import`/`from ... import`) and round-trip opt view-bearing records across the mode boundary (construct in normal, read/rebuild in opt, return); only lifetime/`&mut` (non-rooted) carriers stay refused with a `non-rooted-reference opt type/trait/API` diagnostic; `tests/misc/rfc0167_normal_view_bearing_types.rs` (expected 3553/74, admitted Wasm, zero leaks). **Multi-carrier normal-to-opt evidence, 2026-09-23:** `rfc0167_mode_transport` carries `&String`, `&List(Int)`, and `&[Int]` inside one public opt record through a generic identity and a tuple, while normal owners mutate after the carrier is formed. The independently calculated snapshot result is 46311 in canonical interpretation and admitted unoptimized Wasm; both audit zero leaks. A deeper `&List(List(List(Int)))` payload fails closed at canonical admission. Other unadmitted payload families remain row G gaps. |
| K | Must/once/pure, Dynamic, capabilities and suspension compose; cancellation cannot drop must captures | PARTIAL under the canonical-oracle cut: SEC-094 now rejects `must` transfers that can re-enter through a loop backedge, and SEC-095 rejects root-await suspension that would strand a `must` capture. Task handles are converted to private raw IDs before cancellable scheduler continuations, and the private bridges reject user construction. Async readiness now selects and revalidates exact rooted-view facts for canonical view modules, retaining legacy loan evidence only for legacy inputs; cancellation-fingerprint tampering, stale capture identity, and replayable `once` recipes still reject. The positive resource-composition cases match independent expected traces in both modes in the canonical SafeWIR interpreter and admitted unoptimized Wasm with zero leaks; the existing negatives still reject `Dynamic`, capability and generic duplication, effect/cardinality erasure, replayable async recipes, and cancellable frames with live must/once captures. Broader cancellation and suspension composition remains open. **Views across suspension, 2026-09-22:** an async function refused any view parameter or result with a loan-model reason ("its task can outlive the caller's loan"). RFC-0167 section 7 admits a view across suspension when it has an authenticated retained-root carrier, and every current carrier is one. So async lowering now refuses only retired borrows and lifetime-bearing shells at that boundary, and treats a direct view parameter as live across the task's own `await`. The owner-family closure check, which task continuations use, accepted only the `&Int` owner shape for a captured view; it now also accepts a token-carried whole-root view (`&String`, a record, a scalar List) retained into a managed environment field, bound to exactly those payloads. A scalar-value view (`&Float`) held across a task's own `await` still fails closed at lowering. `rfc0167_async_view` gives 42 (an awaited function reads an `&Int` snapshot), 5.0 (`&Float`), and `spawned`/`3 2` (a spawned task reads `&String` and `&List(Int)` views after yielding) on both engines with zero leaks, and `&'a` stays refused. A view LOCAL held across an `await` in the caller is still refused (unchanged). Admitting views into task recipes exposed BUG-646, a view of a region-born root escaping a normal-mode `region` inside a closure or task recipe (memory-safe on the canonical route, which never bump-allocates, but contrary to section 8). A region whose value type holds a callable or `Task` and whose body forms a view of its own local now refuses the copy-out. Found BUG-645: a `var` local used after an `await` in async `main` loses its receiver type or capture identity, with no views involved. `var` locals across suspension (BUG-645), 2026-09-22:** only `var` *parameters* are barred from spanning suspension, but `var` locals in an async function broke there too. Async lowering carries each live local into the continuation segment as an untyped `own` parameter. Two things went wrong. A method on a carried `List` could not resolve, because the parameter had no type. A `var` reassigned between two suspensions was refused at admission (CapturingClosureBinding), because the next continuation captured the reassigned value and claimed it was the parameter's entry value. The carried parameter now takes the type evident from the local's initializer: literals, lists, tuples, operators, typed locals, module calls, and non-generic constructors. A carried `var` is rebound as a fresh `var` local at segment entry, so the parameter is never reassigned and the authority check is unchanged. `bug645_var_across_await` runs every case in one async program (one compile, inside the gate's per-test budget) and gives `1/6/7/2/6/9/3/2` on both engines with zero leaks. **View locals across `await`, 2026-09-23:** a `&List`, `&String`, or record view held in a local across an `await` failed type checking ("cannot resolve the method call") because the carried segment parameter had no type. Async lowering now types `&place` of a typed local as `&T`. `rfc0167_view_locals_across_await` holds `&Int`, `&List(Int)`, `&String`, and record views across two suspensions, keeps a view's 3-element snapshot while its `var` root grows to 4, and passes a held view into an async function: `42/3/6/7/3 4/3` on both engines with zero leaks. **View results, 2026-09-23 (corrects the 2026-09-22 note):** that note said async view *results* were admitted. They were not: `async fn keep(v: &Int) -> &Int` failed lowering with `UnsupportedType` on the task thunk `fn() -> Task(&Int)`, and an awaited view could not be read. Direct view results now work: the task carries the view's carrier, and the resume value of `let x = f(..).await` is typed with `f`'s awaited type (an `async fn`'s declared result, or `T` for a plain `-> Task(T)`), so methods on it resolve. A `&[T]` parameter is carried into the task recipe by the slice capture (row G). `rfc0167_async_view_results` returns views after a suspension while the caller mutates the root: `&Int` `21 5`, `&[Int]` `2 20 30 100`, `&String` `3 zz`, `&List` `3 4`, record `7 9`. An awaited `&[T]` argument sums to 30, and a spawned task holding a slice prints `2 30` after `spawned 7`. A region-born view returned through `Task(&Int)` or `Option(Task(&[Int]))` is still refused (BUG-646 rule). Evidence is admitted Wasm with zero leaks. **IMPLEMENTED BUT UNVERIFIED (write-only `var` after `await`, 2026-09-25):** a `var` local reassigned after an `await` and never read again previously failed type checking ("assignment to unbound variable") because the continuation did not carry it. The lexical binding walk now counts assignment to an outer binding as a continuation use, so the existing value and write authority ride the segment parameter. The focused compiler build passed; a fresh CLI `check` accepted the previously failing source and its admitted run printed `done`. This is an implementation smoke only: hand-computed admitted Wasm behavior and a zero-leak audit remain open. **Scalar views across `await`, 2026-09-23:** `&Float`, `&Bool`, and `&Duration` views (carried as the value) failed across a suspension: canonical lowering refused the `own` continuation parameter async lowering carries them in, and an async continuation that also captures a capability refused the view's capture as a retained carrier. An `own` scalar-value view is now a plain value lane, and its capture is a copied scalar field. `rfc0167_scalar_views_across_await` holds all three across two yields while their `var` roots change, awaits async functions that yield before reading each, and reads two in a spawned task: `2.5 9.0/true false/true true/1.25/false/true/spawned/task 2.5 true` on admitted Wasm with zero leaks. **`await` nested in expressions, 2026-09-23:** an `await` in a `match` scrutinee, `if` condition, binary operand, interpolation, or call argument, or under `?`, was refused ("`await` in a `match` scrutinee is not yet supported", "`await` must be the whole right-hand side"), and `?` in an async fn failed type checking. Async lowering now hoists each nested `await` into a temporary before its statement, spilling earlier operands first so evaluation order holds. A `?` on an operand whose `Result`/`Option` type is evident becomes a match whose failure arm returns. An `await` in the right operand of `&&`/`||`, or in a branch of an `if`/`match` nested in a larger expression, is refused by name. `rfc0167_await_in_nested_expressions` gives `two/7/pos/10/6/7/stamp 8 before get 9/ok 10/negative -1/some 21/none` in admitted Wasm with zero leaks. **Views through generators and channels, 2026-09-23:** the compiler path admits rooted immutable views as generator parameters and yields, and closed channel messages distinguish a view carrier from its referent value. An unused owner parameter is released after the lambda entry authenticates its closure tag and capture prefix. The surviving `rfc0167_views_through_channels_and_generators` channel case has the independently computed `21 5` result on admitted unoptimized Wasm with zero heap leaks. **IMPLEMENTED BUT UNVERIFIED AT THE CURRENT TIP:** the generator snapshot and region-born generator/send refusal cases that previously supported the same `21 5` and fail-closed claims were removed from that dedicated file by the test-pruning change `e79cd21ac`; `rfc0130_surface` covers generator syntax and reflection, not rooted-view transport. Those generator and region claims need restored focused evidence before being marked proven. Broader message view families remain subject to exact carrier admission. **Dynamic rooted-view result, 2026-09-24:** An ordinary public `@dynamic` method may declare a rooted `&Int` result so its reflected descriptor remains available. An erased `dynamic.call` cannot authenticate that result carrier and returns `DynamicError.BorrowedResult` before decoding arguments or invoking the method. `rooted_view_result_is_reflected_but_dynamic_call_returns_borrowed_result` pins the hand-derived transcript `relations-0` / `borrowed-result-view` in admitted unoptimized Wasm with zero heap leaks; the wrong argument would have failed decoding if the method were invoked. View inputs remain refused at the static `@dynamic` boundary. **Slice subscripts in `async fn`, 2026-09-23:** `s[i]` on a `&[T]` inside any program with an `async fn` failed checking ("in call to `list.at`") because the typed async projection pre-lowered subscripts to user `list.at`, which stays closed to slices; the projection now keeps `Expr::Index`, so the read is the same checked element read as ordinary code. `rfc0167_slice_subscript_in_async` reads `&[Int]`/`&[String]`/`&[Float]` before and after yields, in awaited and spawned tasks, with snapshots kept while roots mutate: `20 40 3 7 5/40 20/cdef/ef!/spawned 100.0/6.0` on admitted Wasm with zero leaks. |
| L | Region copy-out versus opt no-hidden-copy rules; frozen/unique/view interactions; packed admitted layouts | PARTIAL after the §5 revision: SEC-096 now rejects a view of a region-born root escaping rewind and admits a view formed in a region over a parent-owned root. Exact normal copy-out, opt parent-owned promotion, packed result, and fresh-`must` transfer cases match independent expectations in the canonical interpreter and admitted unoptimized Wasm with zero leaks. A supported direct-`&Int` nominal carrier executes while the same shape over a region-born root rejects; 2026-09-23: normal-mode region record/list/`Option`/tuple results carrying views now copy out under syntactic per-field parent-root provenance (`&parent`, `&parent.f`, `&parent[i]`, a parent binding, or a region `let` with the same proof) and refuse any region-born or call-result view (`tests/misc/rfc0167_region_parent_view_results.rs`, admitted Wasm + zero leaks); a parent-owned `&[Int]` range over `[1, 2, 3, 4][1..3]` returns the hand-computed `2 + 3 = 5` through admitted unoptimized Wasm with zero leaks, while the matching region-born slice refuses escape; opt-mode aggregate copy-out still refuses for want of a copy-free proof. Revised frozen/unique/view composition remains open. Old lifetime and exclusive-reference cases are historical only. No optimizer-completion claim. |
| M | Typed-only code generation, authenticated Dynamic and sealed construction; ordinary and generated code reject retired syntax | PARTIAL: compiler-owned string, binary, list, tuple, direct and indirect call, method-call, qualified type call, field, named-record, match, try, lambda, and block builders construct AST directly. Fixed-expression fixtures, JSON aggregate generation, and production derives including generic and cross-module `derive(Deserialize)` use those builders; retired `meta.item`, `meta.expr_raw`, `stmt_raw`, and `block_raw` names occur only in rejection tests. The compile-time evaluator exposes neither `emit(String)` nor `Console`; generated items cross typed `emit_item(ItemSyntax)`. Reflection accepts compiler-issued expression, type, pattern, parameter, statement, block, match-arm, and item handles and rejects source-backed syntax values at builder and item-hole ingress. Unused source-projection and payload-parser helpers are removed. Tagged interpolation carries owned expression AST through expansion, and a string marker is admitted only when the compiler issued it for the active tag. Compiler tag expansion now requires `List(meta.TagHole)` and `meta.expr_tag_hole_typed`; the former `List(String)` tag ABI and public `meta.expr_tag_hole(String)` entry point fail closed. Compiler-owned markers still become expression AST without reparsing source. Existing compiler fixtures use the typed form. The separate Glamour project still contains legacy tag declarations and calls and is outside this compiler-only migration, so this row remains PARTIAL for installed consumers and final integrated evidence. **IMPLEMENTED BUT UNVERIFIED (typed function blocks):** compiler `meta.function_block` now constructs a typed Function AST directly; it no longer parses a fixed source skeleton. This structural change passed a focused `witchy-interp` compile check and does not close row M without later integrated semantic evidence. **IMPLEMENTED BUT UNVERIFIED (typed generation breadth):** user derives now use the typed `emit_item` path, with explicit source-text returns refused; aliases remain checked at the typed emission boundary. The compiler-owned `expr_await`, `stmt_yield`, `block_region`, and `expr_if` builders and async/generator function wrappers construct syntax nodes directly; `expr_if` accepts only sealed expression/block handles and an optional sealed else block; `expr_for` accepts a sealed binding identifier, iterator expression, and body block and refuses call-site binders. `expr_while` accepts only sealed condition and body handles and constructs the loop AST without parsing source. `stmt_assign` accepts only a sealed local name and value expression and constructs an ordinary assignment AST with zero place-rebuild layers. `expr_index` accepts only sealed base and index expressions and constructs an ordinary subscript AST for subsequent source checking. `expr_unary` constructs only fixed arithmetic/logical/bitwise, immutable-view, and dereference unary operators from a sealed operand; `expr_range` constructs half-open or inclusive ranges from sealed bounds. `expr_unary` and `expr_range` have no unoptimized-Wasm or leak evidence yet. The sealed `expr_while_let` constructor structurally desugars a pattern, scrutinee, and body after comptime expansion; a focused `witchy` CLI build and a temporary generated canonical `check` passed, but it has no admitted unoptimized Wasm execution or leak audit. The typed function constructor rejects mixed async/generator flags, suspended `var` parameters, wrong-mode await/yield, suspension in an ordinary nested lambda, and async/generator trait methods before issuing an item handle. Focused compiler builds for the typed function, suspension, `expr_if`, `expr_for`, `expr_while`, `stmt_assign`, and `expr_index` builders passed; `expr_unary` and `expr_range` passed a focused `witchy-interp` build. Temporary generated CLI smokes printed the expected `2` for `expr_for`, `7` for `expr_while`, and `3` for a `stmt_assign` update inside a generated while loop; a temporary `meta.call_site` assignment binder failed closed. A generated `expr_index` subscript printed the expected `9` in the fresh CLI. A temporary generated unary/range program passed canonical `check` and its fresh CLI run printed the hand-computed `-4` after the builder used the compiler's structural range desugaring; its first direct `Expr::Range` form failed closed at unsupported canonical lowering, and the sealed `&` and `*` admission is now implemented but unverified: ordinary checking still decides whether the operand is a rooted place or readable view. Hand-computed admitted Wasm behavior and a zero-leak audit remain pending, so row M stays PARTIAL. |
| N | Independent expectation, canonical SafeWIR interpretation, and admitted unoptimized Wasm agree for every supported row; optimized output optional | PARTIAL: direct, generic, trait-mediated, local function-value, shallow nominal/tuple/`Result` aggregate, direct aggregate-`var` CoW, concrete `Option(&Int)`, and source-linked normal -> opt -> opt rooted `&Int` transport match their independent results in the canonical interpreter and admitted unoptimized Wasm with zero leaks. Returned `&[Int]` transport likewise matches the independent projected length 2 on both engines with zero leaks, while the independent negative invariant refuses an unadmitted `&[Float]` carrier at canonical admission. Generic `first[a](&[a]) -> Option(&a)` now adds the same three-leg evidence for Some=20 and None=1 with zero leaks, while its region-root escape rejects at the checked boundary. The exact-core, unit-result, ordinary and normal-mode unique ownership, frozen/unique, region-promotion, local and cross-module opt-callable provenance, resource-composition, explicit-discard effect, and bounds-revalidation matrices now evaluate independent expectations in the canonical interpreter and admitted unoptimized Wasm instead of the source tree walker. Nested and broader-payload `Result` and tuple/structural matrices remain open. Indexed write-back, recursive calc shared-loop execution, and `String.repeat` static-copy admission also use canonical-route witnesses. No RFC-0167 semantic test invokes the source tree walker on this tip; it is frozen and provides no RFC-0167 acceptance evidence. Add the same three-part evidence with heap/retain audits for remaining carriers, aggregate closure/indirect capture, broader generic slices, and broader projections. A diagnostic or structural rejection needs an independent expected diagnostic/invariant and the exact checked boundary that rejects it. Optimized output is optional corroboration only. Nested `Option` view carriers add three-leg evidence in `rfc0167_nested_option_view`: two shells built, returned, and matched give 7; inner and outer `None` choose their own arms and sum to 1105; three shells relayed through a call give 18; a record with a direct `&Int` beside an `Option(&Int)` field, relayed through `own`, gives 49. All four have zero leaks in both engines, and a region-born root still cannot leave the region inside nested shells. General nested carriers add four more three-leg witnesses in `rfc0167_nested_option_view`: a tuple in an `Option` gives 62; a `Result` in an `Option` gives 403 across its `Ok` and `Err` arms; an `Option` in a tuple and in a `Result` gives 353; a record in a record plus a user sum gives 775. All have zero leaks in both engines. 2026-09-22: `tests/misc/rfc0167_string_view_carrier.rs` pins `&String` relay/return-of-local/snapshot-across-reassignment/generic identity against an independent Int, canonical interpreter and admitted Wasm with zero leaks, plus region-escape and `&Float` refusals. **Contextual try, 2026-09-23:** `e ? "msg"` failed on the canonical route for every operand with `MissingBinding("__try_ctx")`: the type-directed rewrite of the `__try_ctx` intrinsic into `option.ok_or` / `result.map_err` ran only in the legacy codegen front-end, never in canonical source preparation. The rewrite now lives in `witchy_types::try_context` and runs in `canonicalize_callable_instances` before specialization (the legacy front-end consumes the same function). `rfc0167_try_context_and_leading_minus::contextual_try_propagates_option_and_result_with_context` pins `Result` and `Option` operands on the success and error paths, plus nested context, against the independent transcript `ok 5` / `err halving: odd 7` / `ok 40` / `err empty list` / `ok 18` / `err outer: halving: odd 9` in admitted unoptimized Wasm with zero leaks. |
| O | Installed `check`, `fmt`, compile, and run workflows use the canonical route; migrate std/book/examples atomically with compiler cut | PARTIAL after the §5 and runtime revisions: the language spec, mandatory agent briefing, contributor guide, and agent playbook now require independent expectation -> canonical SafeWIR interpreter -> admitted Wasm and forbid new source-tree-walker semantics. Production `run`, `sandbox`, playground, and fixture-mode `test` now execute admitted Wasm; `test --backend interpreter|both` rejects with the canonical-parity migration path instead of exposing the frozen source walker. The focused installed test-runner matrix is 24 passed, 0 failed. Playground validation compares browser compiled Wasm with production admitted Wasm without invoking the source interpreter. Detached raw Wasm is explicitly refused because bytes alone cannot recover canonical admission authority; the corresponding installed regression executes the authenticated calc artifact against an independent expected transcript. Checked structural encoding, layouts, constructor/pattern lowering, and safe `?` reboxing admit the spec's anonymous-union `Result` example. Block 21's accepted `&[a].length()` passes parsing, method resolution, and type checking without exposing `Slice(a)` or a by-value `.at()` escape; the concrete `&[Int]` form now lowers and executes through the authenticated retained `{root,start,len}` carrier. The compiler, generic slice carrier, standard library, remaining book/examples, formatter, diagnostics, and exact-tip installed workflow still require completion evidence. Editor tooling remains non-blocking until the core language contract is green. **Formatter, 2026-09-21:** `witchy fmt` PANICKED on canonical RFC-0167 source that reads a view (`(*value)`), with "cannot format a migration-only dereference as canonical source". A read through a view is canonical; only a write-through deref is retired, and the parser rejects that before the formatter sees it. Three layers had to change: the unary printer (panic), `expr_has_unary_legacy_mutation` (which classed every deref as legacy mutation and made the file unformattable), and the printer's spelling - a bare `*value` beginning a line continues the previous line as multiplication, so the formatter's own reparse guard refused its output. A deref now always prints as `(*value)`, the spelling every RFC-0167 witness uses. Pinned by `format_tests::reading_through_a_view_formats_as_canonical_source`; `fmt --check` over std and examples is unchanged. **Diagnostics and docs, 2026-09-21:** `witchy check` gives a targeted RFC-0167 error for `&mut T`, `&'a T`, a write-through `*r = v`, and returning a `let`-borrowed view; `fn f<'a>(...)` and `fn f<a>(...)` previously fell through to a generic "expected `(`, found `<`" and now say that lifetime parameters are retired or that type parameters are written `fn f[a]`, pinned in `parser_tests::rfc0167_rejects_retired_reference_surface_with_targeted_diagnostics`. `witchy doc` renders `&Int`, `&[String]`, and a `&Int` result verbatim. LSP hover and completion on view signatures are already pinned by `lsp_tests::rfc0167_lsp_reference_signatures_are_canonical` (both modes, no diagnostics, `fn shared(value: &String) -> &String` shown verbatim). **Source migration census, 2026-09-21:** across `std/`, `book/src/`, `examples/`, `projects/`, and `spec/language.md`, no `&mut`, `&'a T`, `View(T, 'a)`, `let('a)`, `local unique`, or `Nil` remains outside the spec's own retirement list and `std/meta.witchy`'s `TView` constructor (which reconstructs `&T`); the last stale reference, a glamour comment calling `&'a T` the view type, now names `&T`. The remaining row O legs are the generic slice carrier and the exact-tip installed workflow. **Line-leading views, 2026-09-21:** a line beginning with `&place` or `*view` continued the previous line as bitwise-and or multiplication, so `let local = 5` followed by `&local` parsed as `let local = 5 & local` ("unbound variable `local`"), and with a bound name would have silently computed a different value. A line-leading `&` or `*` now always begins a new expression; an infix operator continues a line only from the end of the line. No std, example, project, or book source relied on the old reading. Pinned by `parser_tests::a_line_leading_view_or_deref_starts_a_new_expression`. **Line-leading reads after a field, 2026-09-22:** `let v = &p.y` followed by a line `(*v) * 10` parsed as the method call `p.y(*v)` ("no method `y` on `P`"): plain calls already required the argument list on the callee's line, but `.name(` did not. A method call's `(` must now open on the method name's line too, so the canonical `(*view)` read at the start of a line is always a new expression. No std, example, project, or book source split a method call's arguments onto the next line. Pinned by `parser_tests::a_line_leading_paren_does_not_become_method_call_arguments`. **Installed workflow at the exact tip, 2026-09-22:** `rfc0167_installed_workflow` runs the tip binary's `check`, run, `fmt`, and `fmt --check` on one canonical program (record, String, Float, and List views; unit results; `let _ =`; `var` write-back through a loop; a closure capturing a record view) and matches the independent transcript `views/unit/34/6/1/14/103`, and `check` names each retired spelling (`&mut`, `&'a T`, `fn f<'a>`, write-through `*r = v`, `Nil`). Writing it found and fixed a formatter regression: `(*s).length()` was reprinted as `((*s)).length()`, because a deref (always printed parenthesized) still took unary precedence; it is now a primary, pinned by `format_tests::a_read_through_a_view_is_not_parenthesized_twice`, and `fmt --check` over std and examples is unchanged. It also found BUG-644 (`show` cannot dispatch on a value bound from an `if` expression, unrelated to views; the program annotates that binding). The remaining row O leg is the generic slice carrier. **Generic slice carrier, 2026-09-22:** the generic slice carrier leg is closed. `&[T]` for Float, Bool, Duration, and String elements lowers through canonical SafeWIR on the same retained `{root, start, len}` carrier as `&[Int]`, with no copy of the sub-list. The evidence is recorded in row G and pinned by `tests/misc/rfc0167_generic_slice_view.rs`. |
| P | For every optimization-readiness row in section 10, map positive/negative witnesses to checked facts, invalidation rules, and cross-boundary summaries/IR; preserve representation freedom without adding passes | PARTIAL: checked view facts and canonical lowering preserve authenticated root identity and projection for direct transport, and the CoW structural witness forces a distinct allocation while a view is live. Local callable metadata retains the exact rooted payload/carrier identity across construction and indirect invocation; shallow direct-`&Int` nominal, tuple, `Option`, and `Result` carriers retain a source-derived opaque owner identity and balance their roots; and async readiness authenticates the rooted-view fact product rather than reconstructing lexical loans. External/open view ABIs remain fail-closed. Range/retain obligations, joins, nested/broader aggregate fields, generic/trait summaries, and explicit retain/copy-elision positive plus near-miss witnesses remain open. No timing, allocation-improvement, or optimizer-completion claim. |
| Q | Freeze and retire the source tree walker without losing compiler services | PARTIAL, with the freeze effective immediately: RFC-0167 views were not added to the source interpreter, every RFC-0167 semantic test has removed source-tree-walker execution, production parity uses the canonical interpreter as oracle, and playground validation no longer depends on source execution. The installed fixture test runner now accepts only admitted Wasm; its former `interpreter` and `both` source-backend selections reject. The executable `witchy --bench` path now compares canonical SafeWIR interpretation with admitted Wasm rather than exposing source-tree-walker execution. Mandatory contributor and agent workflow documents no longer require source-interpreter parity or carry interpreter-debt tables. Preserve comptime/build evaluation only as a temporary compatibility service and move linker/tooling pipeline support to a runtime-neutral crate. Compatibility and security repairs may maintain those bounded uses but do not count as language semantics. Canonical lowering of comptime/build code and deletion of the tree walker are the end state; no new dependency on the tree walker may be introduced. Production static-site evaluation in the source tree-walker freeze inventory remains outstanding; row Q is not complete. |

## Exact-tip evidence checkpoint

Row G lowerer storage-plan consumption (architecture implemented, semantic
evidence unchanged, 2026-09-25): the direct rooted-view gate and checked
slice-element extraction now consume `StoragePlan` from canonical-source.
The lowerer preserves the direct-slice syntax and frozen-payload checks and
converts only proven owned or non-Unit inline scalar elements into slice
carriers; Unit remains refused. Adversarial review found and corrected that
Unit near miss and found no other changed admission in direct views, nested
records, qualifiers, or generic slices. A focused `witchy-lower` compile
passed. This does not yet replace the
remaining layout, checker, and SafeWIR classification sites; A–O evidence
and row status are unchanged.

Row G `&[T].owned()` element judgment (architecture implemented, semantic
evidence unchanged, 2026-09-25): type checking now consumes the shared
`StoragePlan` for concrete slice elements. Only a recursive owned carrier or
non-Unit inline scalar may materialize `List(T)`; the existing declared
generic type-variable allowance is unchanged. Unit, qualifiers, view-bearing
aggregates, unsupported nominals, and over-budget shapes remain refused.
Adversarial review found no authority widening. This consolidates one checker
gate, and a focused `witchy-types` compile passed. Admitted Wasm, zero-leak
evidence, and A–O status are unchanged.

Row G structural rooted owner identities (architecture implemented, semantic
evidence unchanged, 2026-09-25): the scalar-place and rooted-slice owner/list
ID pairs now derive from a domain-separated digest of the typed storage class
and recursive `CarrierElement` identity, rather than from their position in
a sorted list. A truncated-ID collision between materialized families fails
layout construction; SafeWIR also checks owner-ID uniqueness across family
classes. Adversarial review found no new authority path or assumption that
rooted IDs are contiguous. Existing closure and aggregate owner allocators
remain ordinal and are a separate structural-identity gap. The change has no
new admitted Wasm or zero-leak evidence and closes no A–O row.

Row G rooted-slice layout request (architecture implemented, semantic
evidence unchanged, 2026-09-25): layout projection now asks the shared
`StoragePlan` for a checked slice element's recursive owned carrier, or a
non-Unit inline scalar, before registering its rooted-slice family. Qualified
elements, view-bearing aggregates, and Unit still refuse this conversion;
the physical layout gate remains separate. Adversarial review found no
authority widening or prior positive-case loss, and a focused
`witchy-lower` compile passed. This removes one direct
carrier-parser call site, but the broader list/aggregate layout catalog is
not yet unified with the shared plan. No admitted Wasm or zero-leak evidence
or A–O closure is claimed.

Row G WIR rooted-view layout judgment (architecture implemented, semantic
evidence unchanged, 2026-09-25): source/view physical correspondence now
consumes the shared `StoragePlan` before matching checked aggregate,
structural tuple/Dict, and recursive List roots. The existing record-head
priority, independent physical layout checks, and final checked-source
signature binding remain. Adversarial review found no authority widening;
the new bounded source preflight can only conservatively refuse an edge
case at the prior depth limit. The broader non-view and owner-family gates
still have parallel classifications. No new Wasm, zero-leak evidence, or
A–O closure is claimed.

Row G shared source-storage entry point (architecture implemented, semantic
evidence unchanged, 2026-09-25): `StoragePlan` now composes checked scalar,
recursive owned-carrier, rooted-view, and view-bearing aggregate plans from
the same canonical-source module. `CanonicalSourceView` supplies its exact
checked definitions and nominal ordinals; callers must still establish source
membership and physical layout correspondence. The checker capture gate and
WIR record-root signature gate consume this entry point. The classification
refuses unsupported qualifiers, unproved ownership, and source/definition
graphs exceeding 128 levels or 4096 visited nodes before aggregate traversal.
This consolidates two call sites but does not yet make every type-checking,
lowering, layout, and admission path consume one plan. Existing forms remain
the admitted boundary. Focused canonical-source, checker, and WIR compilation
and all 17 existing canonical-source unit tests passed; adversarial review
found no remaining authority blocker. No new Wasm or zero-leak evidence or
A–O closure is claimed.

Row G aggregate view-carrier field plan (architecture implemented, semantic
evidence unchanged, 2026-09-25): the recursive carrier admission walk now
returns a typed plan with the checked aggregate shape and a plain or carried
classification for each field. Owner identity consumes the plan to find
retained projections instead of repeating a separate slice search. The
existing packed-layout refusal, eight-level bound, direct-slice record rule,
view containment check, and independent SafeWIR layout validation remain.
A canonical-source compile, its 19 existing tests, and a lowerer compile
passed; adversarial review found no new authority path. Boolean callers now
allocate the small field plan, so compiler cost should be monitored. No
new admitted form, Wasm evidence, or A–O closure is claimed.

Row G callable view capture through shared storage classification
(architecture implemented, semantic evidence unchanged, 2026-09-25): the
checker now asks `RootedViewStorage` for the checked payload class instead
of keeping separate scalar, slice, record, and List admission predicates.
The class gate preserves the prior refusal of qualified scalar/String,
direct tuple/dictionary, and non-private slice payloads. A focused checker
compile and one existing recursive-list closure test passed; adversarial
review found no widened capture path. The diagnostic now names recursive
slice carriers accurately. No new semantic form, admitted Wasm evidence,
or A–O closure is claimed.

Row M typed view and dereference unary builders (implemented but unverified,
2026-09-25): the sealed `meta.expr_unary` constructor now admits exactly `&`
and `*` alongside its existing arithmetic/logical operators. A fresh
generated-source CLI smoke checks and prints the hand-computed `4` through
`(*(&xs))[0]`; `&mut` still refuses at the builder whitelist. The focused
`witchy-interp` build passes. No admitted unoptimized Wasm and zero-leak audit
has been recorded for this generated path, so row M remains partial.

Row M typed destructuring let (implemented but unverified, 2026-09-25):
`meta.stmt_let_pattern` constructs `LetPattern` directly from sealed pattern
and expression handles. The existing source checker retains responsibility for
irrefutability and binding validation; unsupported or source-backed handles
fail closed. A focused `witchy-interp` build with the test profile passed.
A temporary generated tuple-destructuring program passed fresh CLI `check`
and `run` printed the hand-computed 5. Admitted unoptimized Wasm and zero-leak
evidence remain pending, so row M stays open.

Row M typed loop control (implemented but unverified, 2026-09-25):
`meta.stmt_break` and `meta.stmt_continue` use the existing sealed
statement-leaf bridge with fixed compiler-owned variants. Other leaf names
still fail closed, and canonical lowering determines whether a loop is in
scope. A focused `witchy-interp` build with the test profile passed. Generated
CLI execution, admitted unoptimized Wasm, and a zero-leak audit remain pending;
this does not close row M.

Row M typed numeric and duration literals (implemented but unverified,
2026-09-25): `meta.expr_float` constructs a finite `Expr::Float` and
`meta.expr_duration_ms` constructs an exact `Expr::Duration` through the
existing sealed expression-leaf bridge. Invalid payloads and non-finite Float
values fail closed. A focused `witchy` build passed, and a fresh CLI `check`
and `run` of two generated functions printed the hand-computed 11. No
admitted unoptimized Wasm witness with a zero-leak audit has been recorded,
so row M remains open.

Row M typed parameter conventions (implemented but unverified, 2026-09-25):
`meta.param_let`, `meta.param_var`, and `meta.param_own` construct sealed
parameter handles with the exact source convention; the private bridge accepts
only fixed convention tokens. Generated function checking preserves explicit
`let` no-escape and refuses suspended `var` parameters. A temporary generated
program printed the hand-computed `5/6/3` for `var` write-back, `own`, and
explicit `let`, and canonical interpretation agreed with admitted Wasm. The
temporary smoke has no zero-leak audit or durable witness, so row M remains
open.

Row G bounded nested-list carrier (2026-09-24):
`tests/misc/rfc0167_nested_list_root_view.rs` pins three distinct whole-root
`&List(List(Int))` paths with hand-computed results: direct relay and nested
mutation preserve the old two-row root while the current owner grows (4239);
a closure retains old children 1 and 3 after mutation (49); and both branches
of a generic two-root choice retain their selected snapshots (1398). Each
result matches admitted unoptimized Wasm with `Some(0)` canonical heap leaks.
A region-born root refuses escape at checking, while a qualified inner list
and a third List shell refuse canonical admission. This closes the exact
whole-root child-ownership gap only; `&[List(Int)]`, other nested payloads,
and broader row G carriers remain open. Exact-tip presubmit and queue gate
are pending.

Row G recursive whole-list root (implemented but unverified, 2026-09-25):
the checker, canonical ABI identity, lowering, and SafeWIR layout comparison
now use the shared recursive `CarrierElement` relation for whole `&List(T)`
views. The root must be a retained list, and every nested managed edge must
have the checked owned slot, RC header, and retain/clone/release behavior.
This supersedes the historical third-shell refusal above without changing its
recorded evidence for the exact two-shell case. A temporary depth-three smoke
computed `7 * 10 + 9 + 2 = 81`; admitted Wasm printed 81 and parity agreed
with the canonical interpreter. This is an implementation smoke, not acceptance
evidence: no durable witness or zero-leak audit covers the new depth. Row G
remains partial.

Row G recursive tuple elements (implemented but unverified, 2026-09-25):
the checked-source carrier relation now accepts nonempty structural tuples
whose fields recursively satisfy the same scalar, String, List, or tuple
ownership proof. SafeWIR requires an owned packed-record pointer and matches
every field's physical slot in order; qualifiers, nominal lookalikes, and
unproven ownership edges still fail closed. Focused compiler builds passed.
Temporary shallow and nested List slice programs compiled and ran in admitted
Wasm with the hand-computed exit value 9; one canonical interpreter parity
probe agreed. These probes have no recorded zero-leak audit or durable Wasm
evidence, so they close no row G acceptance gap yet.

Row G structural dictionary elements (implemented but unverified,
2026-09-25): the same recursive carrier relation accepts `Dict(K, V)` only
when its physical layout is an owned record of two retained List fields and
both `K` and `V` have recursively valid ownership operations. Temporary
admitted Wasm runs returned the hand-computed 8 for a shallow dictionary
slice and its `.owned()` materialization, and 16 for a dictionary with a
nested List value copied through `.owned()`. No durable hand-computed Wasm
witness or recorded zero-leak audit closes the row yet.

Row G structural whole-root tuple and dictionary views (implemented but
unverified, 2026-09-25): checked ABI identity, lowering, and witnessed SafeWIR
now use the recursive `CarrierElement` relation for whole tuple and dictionary
roots. SafeWIR requires a retained packed-record root and recursively checks
each field, including owned retained List slots for dictionary keys and
values. Temporary admitted-Wasm programs returned the hand-computed 14 for
the existing `&(Int, String)` case, 6 for `&(List(Int), String)`, and 21 for
`&Dict(String, List(Int))`. A tuple containing `&String` still refused at
canonical lowering. These smokes have no durable zero-leak audit, so they
close no additional row G gap. The local tuple and dictionary rooted-view ABI
payload keys now use a versioned structural digest; stale compiled ABI
sidecars with the earlier exact-shape digests must be rebuilt and cannot bind
as authority against the new checked manifest.

Row G carrier proof transport (architecture implemented, 2026-09-25):
each rooted-slice owner family now carries its recursive candidate element
proof. The source-independent SafeWIR checker bounds that proof and matches
its physical layout; the checked-source binding separately requires exact
agreement with the parsed source type and membership in
`CanonicalSourceView`. Raw candidates with an unsupported source type or an
oversized proof fail closed. This adds no new admitted source form and closes
no acceptance evidence gap by itself.

Row G typed record-root ABI identity (architecture implemented, 2026-09-25):
checked types and source annotations now build one bounded structural
`RecordRootIdentity` in `witchy-canonical-source`. Its scalar, String, List,
Option, Result, and declared nominal nodes replace the two handwritten
record-root encoders in the checker. Nominal ordinals still come from the
checked frontend; List nodes use the shared recursive carrier proof. The
encoding preserves admitted v3 record-root keys and adds no source form.
SafeWIR record layout and ownership checks remain separate consumers to
consolidate, so this change closes no row G evidence gap.

Row G record-root plan transport (architecture implemented, 2026-09-25):
`CanonicalSourceView` now derives the nominally bound `RecordRootPlan` from
the checked module. SafeWIR requires that plan for every record-root view in
an authenticated callable signature, and its layout correspondence reads the
same plan's field and tag shape before checking the physical record slots.
The source-independent check has no nominal authority of its own; the checked
view supplies it during binding. This tightens admission without adding a
source form or closing any row G semantic evidence gap. Temporary admitted-Wasm
smokes of `&Option(List(List(Int)))` and a declared record with the same List
field each returned the hand-computed value 4; `&Option(List(&Int))` still
refused. No durable zero-leak audit was added.

Row G declared record slice elements (implemented but unverified,
2026-09-25): the shared carrier contract now derives a one-variant heap
record's identity and instantiated fields from checked type definitions.
Every field recursively needs a valid carrier and an owned physical slot;
tagged, packed, must-consume, capability-bearing, recursive, and unproved
forms remain fail-closed. The raw SafeWIR check bounds and matches a
candidate's source syntax and layout, while witnessed binding independently
rederives its exact fields from `CanonicalSourceView` and requires manifest
membership. Temporary `&[Point]` and whole `&List(Point)` `.owned()` programs
passed `witchy check` and each exited admitted Wasm with the hand-computed
value 9.
This smoke does not close row G: its durable Wasm and zero-leak evidence
remain outstanding.

Row G record slice callable transport (implemented but unverified,
2026-09-25): a checked `fn(&[Point]) -> Int` value can now pass through an
indirect callable parameter while the slice's recursive record carrier keeps
its source type, closure layout, and ownership operations aligned. Closure
identity derives the slice element from checked definitions instead of a
scalar or nested-list name list. A temporary `Point(Int, String)` program
passed `witchy check` and exited admitted Wasm with the hand-computed value
9. No durable zero-leak audit or A–O closure is claimed for this path.

Row G direct record slice field (implemented but unverified, 2026-09-25):
the aggregate owner predicate now accepts one direct `&[T]` field in a
one-variant untagged record when the recursive checked carrier proves `T`.
Nested or unproved slices and tagged wrappers still fail closed. Temporary
record-field construction, parameter, direct return, and indirect return
programs passed `witchy check`; the indirect `Point(Int, String)` program
exited admitted Wasm with the hand-computed value 9. This has no durable
zero-leak audit and closes no row G acceptance gap.

Row G whole-root scalar-list expansion (implemented but unverified,
2026-09-24): the compiler now recognizes exact `&List(List(S))` for
`S = Float`, `Bool`, or `Duration` through the shared source predicate,
canonical callable identity, and lower root family. The qualified inner-list
and region-escape refusals still apply. No hand-computed result, admitted
unoptimized Wasm run, or zero-leak audit has been recorded for these new
scalar cases, so this expansion closes no additional acceptance gap yet.

Row G whole-root String-child list expansion (implemented but unverified,
2026-09-24): exact `&List(List(String))` now has a distinct shared checked/source
ListRoot identity. Both retained List headers and each owned String child
remain subject to SafeWIR layout correspondence. Qualified children, deeper
List shells and forged nominal spellings remain refused.
No hand-computed result, admitted unoptimized Wasm run, or zero-leak audit has
been recorded for this new shape; row G remains open.

Row G slice owned-list materialization (PROVEN for exact `List(Int)`, 2026-09-25):
The checked `.owned()` operation accepts exact `&[List(Int)]` and returns
`List(List(Int))`. Canonical lowering copies only the authenticated slice range
with the existing `CloneRange` owner transfer; its pointer slot is `Owned`, so
the `OwnedNullable` backend boundary remains closed after adversarial review.
The existing three-case `rfc0167_slice_owned_materialization` shard passed:
the new nested-list path gives the hand-computed 7306 in admitted unoptimized
Wasm with zero heap leaks; `List(Float)` and must-consume `Ticket` elements
refuse at source checking. Other list element types remain fail-closed. This
closes the exact materialization gap only; row G remains partial.

Row G remaining scalar-list slice materialization (proven for these exact
shapes, 2026-09-25): `.owned()` accepts exact `&[List(Float)]`, `&[List(Bool)]`,
and `&[List(Duration)]` using the same owned-pointer `CloneRange` path as the
proven `List(Int)` case. The existing three-case
`rfc0167_slice_owned_materialization` shard passes at this tip: the three
copies contribute the hand-computed 1 + 10 + 100 = 111 in admitted unoptimized
Wasm, with zero heap leaks. At that checkpoint, `List(String)` and must-consume
`Ticket` elements refused at source checking; deeper and qualified children
remained outside the admitted boundary. Row G remains partial.

Row G String-list slice materialization (proven for this exact shape,
2026-09-25): exact `&[List(String)].owned()` uses the existing owned-pointer
`CloneRange` path to produce `List(List(String))`. The checker and lowerer
retain exact element-family gates. The existing three-case
`rfc0167_slice_owned_materialization` shard passes at this tip: the retained
view, copied list, and changed original contribute the hand-computed
3000 + 100 + 10 + 5 = 3115 in admitted unoptimized Wasm with zero heap leaks.
`List(List(Int))` and must-consume `Ticket` elements refuse at source checking;
qualified children remain outside the admitted boundary. Row G remains partial.

Row G String-child list slice expansion (implemented but unverified,
2026-09-24): exact `&[List(String)]` has a distinct checked/source slice
identity and a retained outer root. SafeWIR requires an owned pointer to a
retained inner List header, then owned String pointers to runtime buffers.
Qualified and deeper child lists, slice `.owned()` materialization, and
closure capture remain refused. No hand-computed result, admitted unoptimized
Wasm run, or zero-leak audit has been recorded; row G remains open.

Row G recursive slice-carrier implementation (implemented but unverified,
2026-09-25): the checker, callable ABI, layout projection, lowering and
SafeWIR now share a source-authenticated recursive element contract through
`CanonicalSourceView`. A checked formation or finalized closed callable
signature supplies the exact structural element; SafeWIR requires its scalar
or owned String layout, and for each List layer an owned pointer and retained
List header. Carrier IDs are allocated without collisions from the sorted
canonical elements and carry no source authority. `.owned()` returns a fresh
`List(T)` through the authenticated projection. Unsupported qualifiers,
nominal lookalikes and elements without retain, clone and release remain
fail-closed. Earlier concrete results remain evidence only for their exact
shapes; the newly generic depths are not marked proven until a hand-computed
expected result, admitted unoptimized Wasm, and zero-leak audit pass. The
queued concrete nested-Int branch went red on an obsolete depth refusal and
did not land; this structural implementation supersedes it. Row G and the
remaining A–O acceptance rows stay open where their evidence is incomplete.
Earlier depth and `.owned()` refusals in this row describe historical
checkpoints; they are superseded for recursively proven slice elements.
A focused compiler smoke with `&[List(List(Int))].owned()` printed the
hand-computed `102` on admitted Wasm, and parity compared that one output line
with the canonical interpreter. No zero-leak audit is recorded for this new
recursive depth, so this does not close an acceptance row.

Row G String-valued dictionary expansion (implemented but unverified,
2026-09-24): exact whole-root `&Dict(String, String)` now has a distinct
checked/source DictRoot identity. The retained dictionary owns its String
value children through the admitted dictionary layout. Qualified, nested,
and view-valued dictionary values still refuse canonical admission; the
existing negative fixture now checks the nested value boundary. No
hand-computed result, admitted unoptimized Wasm run, or zero-leak audit has
been recorded for this new shape; row G remains open.

Row G tuple scalar/String expansion (implemented but unverified,
2026-09-24): the compiler recognizes exact whole-root `&(S, String)` for
unqualified `S = Float`, `Bool`, or `Duration`, alongside the previously
admitted and evidenced `&(Int, String)` carrier. These forms reuse the
retained tuple root and owned String child; closure capture remains refused
until it has a separate checked witness. No hand-computed result, admitted
unoptimized Wasm run, or zero-leak audit has been recorded for the new
scalar cases, so this expansion closes no additional acceptance gap yet.

Row G two-String tuple expansion (implemented but unverified, 2026-09-24):
exact whole-root `&(String, String)` now has a distinct checked/source
TupleRoot identity. Both String fields remain owned children of the retained
tuple. Qualified, nested, and view-bearing fields remain refused, as does
closure capture. No hand-computed result, admitted unoptimized Wasm run, or
zero-leak audit has been recorded for this new shape; row G remains open.

Row G direct List-child RecordRoot expansion (implemented but unverified,
2026-09-24): eligible non-packed `Option`, `Result`, and declared user roots
now share a checked/source ABI identity node for an exact direct `List(S)`
child, with unqualified `S = Int`, `Float`, `Bool`, `Duration`, or `String`.
`&Option(List(Int))` is one instance. SafeWIR's Required-header List guard
must land first: its layout walk reaches the owned child and otherwise could
accept an elided header. Existing RecordRoot identity bytes are unchanged.
Qualified, nested, and owner-bearing List elements remain outside the exact
admission predicate. No hand-computed result, admitted unoptimized Wasm run,
or zero-leak audit has been recorded for this new family; row G remains open.

Row G recursive List children in record roots (implemented but unverified,
2026-09-25): checked `Option`, `Result`, and declared record roots now derive
their List-child ABI identity from the same recursive `CarrierElement` proof
used by slice and whole-list carriers. A versioned payload key keeps earlier
exact-shape sidecars from binding as authority. The shared carrier guard still
refuses qualified, view-valued, cyclic, or otherwise unproved children; the
existing SafeWIR record-layout walk must also match every owned physical slot.
The preceding exact-shape boundary is historical and is superseded by this
implementation. Temporary `&Option(List(List(Int)))` and declared `&Box`
with a `List(List(Int))` field each returned the hand-computed value 4 on
admitted Wasm. `&Option(List(&Int))` refused at canonical ABI checking. These
smokes have no durable zero-leak audit, so no new row G claim is closed.

Row G rooted-view storage classification (implemented but unverified,
2026-09-25): inferred and declared rooted-view callable ABIs now use one
typed `RootedViewStorage` classification in `witchy-canonical-source`. It
composes the recursive carrier proof for list, slice, tuple, and dictionary
storage with the checked record-root plan for Option, Result, and declared
records or sums. `CanonicalSourceView` exposes that classification to SafeWIR's
record-root signature gate. The local rooted-view ABI now derives one v4
structural payload key and one of four physical representation classes instead
of selecting per-shape digest domains and carrier IDs. This intentionally
changes local ABI keys; stale sidecars must fail the exact checked binding.
Checked and source ABI encoders now enforce a 128-depth and 4,096-node bound
before recursive encoding or AST construction; the shared carrier parser
retains its own bounds.
Lowering and layout admission still have separate type-shape branches to
consolidate. This is an architecture change, not new semantic evidence: the
existing carrier forms retain their prior proofs, and no A–O row closes here.

Row G lowerer consumption of the shared storage plan (implemented but
unverified, 2026-09-25): the direct slice/List carrier prefilter and the
indirect-call rooted-view prefilter now read `RootedViewStorage` instead of a
second per-shape admission list. The direct slice's exact qualifier rule and
the List root's Frozen-only rule remain explicit. Other lowerer construction
branches and SafeWIR layout correspondence still need consolidation. This
prefilter refactor adds no semantic evidence or A–O closure.

Row G SafeWIR layout consumption of the shared plan (implemented but
unverified, 2026-09-25): direct record, tuple/dictionary, and whole-List view
correspondence now dispatches from one `RootedViewStorage` classification.
The record-head check still rejects a missing checked plan before fallback;
the physical walks still require the exact record fields, retained aggregate
layout, or required-header packed List with recursively matched elements.
The separate scalar, slice, and nested owner-family correspondence branches
remain to consolidate. This refactor does not close a semantic acceptance row.

Row G SafeWIR source-shape admission (implemented but unverified,
2026-09-25): the whole-root signature and capture predicate now asks the
shared `RootedViewStorage` classifier for inline scalar or retained-root
representation instead of enumerating String, List, tuple, dictionary, and
record heads itself. Scalar-place and slice-projection views still use their
separate authenticated paths. Checked-source nominal ordinals remain required
at final ABI binding. No new carrier behavior or A–O evidence is claimed.

Row G tagged child carrier composition (implemented but unverified,
2026-09-25): the shared recursive `CarrierElement` relation now derives
`Option(T)`, `Result(T, E)`, and checked declared sums from their instantiated
variant fields. SafeWIR requires an exact owned or nullable-owned pointer
slot, an Int tag, the flattened payload count, and a recursive layout and
ownership match for every child. The finite depth and node budgets and
must-consume/capability refusals remain in force. Temporary compiler smokes
admitted `List(Option(Int))`, `List(Result(Int, String))`, and a declared
`List(Shape)` with Int/String payload variants; each retained an old root
across list mutation and returned the hand-computed 23, 23, and 34 on Wasm.
`List(Option(&Int))` failed closed at callable ABI admission. These temporary
smokes have no durable zero-leak audit or full A–O evidence; no row closes.

Row G tagged constructor identity (implemented but unverified, 2026-09-25):
the shared Sum carrier key now includes each checked variant name as well as
its ordered field plans. Equal physical slots with different constructor tag
meanings have distinct structural identities. This changes only newly added
Sum keys; it adds no semantic evidence or A–O closure.

Row G rooted representation classes (implemented but unverified,
2026-09-25): checked rooted views now expose scalar place, inline scalar,
retained projection, or retained whole root as the outer storage class.
The whole-root plan carries byte-buffer, retained sequence, structural
record, or checked aggregate facts. Lowering, source ABI admission, and
SafeWIR's physical correspondence consume this same typed classification.
The existing v4 structural ABI bytes, source admission, qualifier rules,
and independent SafeWIR layout checks are preserved. This consolidation
adds no new semantic evidence or A–O closure.

Row G qualified record-root identity (implemented but unverified,
2026-09-25): the checked record-root structural key now distinguishes
`frozen` and `unique` children rather than erasing their source contracts.
Closed callable manifests restore direct annotated rooted-view spelling only
after a bounded match to the finalized erased type. Qualified children
without an admitted carrier still fail closed, and nested or outer-wrapped
qualified rooted views remain refused because their callable encoder cannot
yet represent those qualifiers. One focused ABI authority pin covers direct
key separation and both refusals. This closes no semantic acceptance row.

Row G structural rooted aggregate result relation (architecture implemented,
semantic evidence unchanged, 2026-09-25): SafeWIR now checks an admitted
`Option(&Int)` result through the same checked, positional ownership-state
tree used by other rooted aggregates. The separate recursive Option-specific
matcher was removed. An existing checked-source case passes through the
generic relation, a focused WIR compile passed, and adversarial review found
no new authority path: missing or different child state still refuses, while
the independent owner-family and source-ABI checks remain. No new admitted
form, zero-leak witness, or A–O row closure is claimed.

Row G owner-carried view storage class (architecture implemented, semantic
evidence unchanged, 2026-09-25): aggregate field recognition and canonical
owner-view identity now consume one source-typed `OwnerCarriedViewPlan` built
from `RootedViewStorage`. Its authenticated scalar-place and retained
projection cases retain their separate physical requirements. An outer
`frozen` slice can still normalize for identity but cannot enter the
aggregate-field admission path; extra nested qualifiers now refuse instead
of passing the old scalar syntax shortcut. A bounded nominal-graph preflight
rejects recursive carrier elements before a carrier and aggregate shape query
can restart each other's budget; an assertion in the existing recursive-shape
test pins the self-slice case. Existing canonical-source tests (19), a lowerer
compile, and the slice-record smoke (2) passed; adversarial review found no
new authority path. No new form or A–O evidence is claimed.

Row G SafeWIR owner-view leaf consumption (architecture implemented,
semantic evidence unchanged, 2026-09-25): the scalar-place and retained
projection source-ABI gates now consume `OwnerCarriedViewPlan` rather than
reparse Int and slice spellings. Final owner-family correspondence compares
the plan's exact admitted projection child with the checked family carrier;
frozen slices and unsupported nested qualifiers remain refused. A focused
WIR compile passed and adversarial review found no new family or source
authority path. No new semantic form, zero-leak evidence, or A–O closure is
claimed.

Row G transitive nominal rooted-view key (architecture implemented, semantic
evidence unchanged, 2026-09-25): the checked nominal root key now binds its
instantiated constructor names and field types. A bounded structural walk
also binds nested nominal declarations, with an explicit back-edge marker
for recursive references. The checked-source witness and physical layout
checks still decide authority; this key change grants no new type or storage
class. One existing ABI test now compares direct constructor, field, and
nested declaration changes. Focused type-checker testing passed; admitted
Wasm and a zero-leak audit were not added, so row G remains partial.

Row G rooted value storage through the recursive carrier proof (architecture
implemented, semantic evidence unchanged, 2026-09-25): checked scalar,
String, tuple, and dictionary root views now select their physical storage
class from `CarrierElement` rather than a parallel source-name list. Slice,
whole-List, and checked aggregate paths keep their prior child budgets and
qualifier gates. The shared classifier still requires independent checked
source authority and SafeWIR physical layout matching. A canonical-source
compile and its 19 existing tests passed; adversarial review found no new
authority path after a depth-bound regression was corrected. No new
admitted form, Wasm evidence, or A–O closure is claimed.

Row G plain List element layout through the recursive carrier proof
(architecture implemented, semantic evidence unchanged, 2026-09-25):
SafeWIR's plain List layout correspondence now consumes `CarrierElement`
when the checked element has a recursive scalar or managed proof. A proven
element must match its exact scalar kind or owned pointer slot, RC-required
nested Lists, and recursive record/sum fields; a physical mismatch cannot
fall through to the older layout walk. Unit and unclassified elements keep
their prior fail-closed path. A focused WIR compile, an existing normal-to-opt
String/List/slice/tuple runtime smoke (1 passed), and adversarial authority
review passed. This is one layout correspondence consolidation, not new
admission or A–O semantic closeout.

At `adf1d01c7` (2026-09-18), the integrated core smoke was **42/42 passed**: rooted views (13), unique/write-back (12), normal/opt interoperation (2), resource composition (5), and optimization readiness (10). This is core canonical-route evidence only; it does not include the source interpreter, full, end-to-end, or examples suites.

At `2f62f4038` (2026-09-19), the newly integrated concrete `Option(&Int)` evidence is **6/6 passed**: four lower carrier/tamper/`var` checks, one exact authority payload-state check, and one production-runtime three-leg fixture (`790`, zero leaks). The three generic rooted-slice fixtures are **1 passed / 2 failed**: region-root escape rejects as expected, while both positive forms stop at the explicit missing authenticated slice-element projection. This is a live failure checkpoint, not an acceptance closeout.

At `fb498bef6` (2026-09-19), the generic rooted-slice fixtures are **3/3 passed** on the integrated tip: Some=`20` and None=`1` agree with their independent expectations in the canonical interpreter and admitted unoptimized Wasm with zero leaks, and the region-born-root escape rejects. Direct rooted-slice, direct `Option(&Int)`, wrong-family refusal, exact owner-field authority, typed Wasm scratch, and plan/emitter correspondence regressions also pass. This closes the generic `Option(&a)`-from-`&[a]` child-projection gap only; broader aggregate/callable/cross-mode acceptance remains open.

At `b5342796e` (2026-09-19), the one broad exact-tip census ran all **5,551** selected tests in 4,332.351 seconds: **5,034 passed, 510 failed, 7 timed out, and 19 skipped**. The largest production cascade is `OwnedReleasePlacement("IncompatibleOwnedEdge(BlockId(27))")`, which prevents the embedded coven from compiling and consequently blocks package-manager, examples, capability, publishing, and installed-workflow tests before their intended assertions. Independent roots include missing bounds guards, dominance/extent failures, adapter/signature mismatch, ownership and write-back failures, unsupported canonical `Rand` layout, missing bindings, and stale lifetime/exclusive/source-interpreter fixtures. These totals are a failure census, not acceptance evidence; optimization-counter failures remain deferred while semantic/runtime blockers and superseded fixtures are separated and repaired.

At `d9f18adb7` (2026-09-19), bounded production-path repairs have advanced the exact fresh-cache `pm_coven_lifecycle::scaffold_and_run` sentinel past the census's `OwnedReleasePlacement(BlockId(27))` cascade and the subsequently exposed `string.rfind` `ValueDoesNotDominateUse(FunctionId(442), v85, BlockId(4))`. Static reference ownership tokens now survive joins without acquiring a destructor, invariant String roots remain dominating SSA values while authenticated scalar metadata crosses `for` and `while` headers, and the hand-built read-only `string.find` CFG discards path-local header facts at its exit. The production sentinel proves that F442 no longer blocks canonical admission, but the attempted direct unit regression is itself red because its scratch module cannot call private `string.find`; an authorized regression shape is still required. The sentinel remains red at the next independent root, `InvalidExtent { FunctionId(473), object v1 }` in `toml.inline_get_array`; therefore none of the broad rows blocked by the earlier cascade are closed and no new broad pass count is claimed.

At `ebda3a1ec` (2026-09-19), the exact fresh-cache `pm_coven_lifecycle::scaffold_and_run` sentinel is **1/1 passed** in 120.54 seconds. The F473 repair reauthenticates an unchanged String root's physical header before carrying fresh scalar metadata over each loop edge; the root itself remains a dominating SSA value and never acquires a loop phi. A minimized nested `if let` + String equality + mutable List-rebind regression passes initial and final SafeWIR and structurally pins that root/metadata separation. This closes the three concrete compiler blockers exposed by that sentinel, but it is one production sentinel rather than a new broad census; the larger missing-bounds, ownership/write-back, adapter, and carrier-scope rows remain open.

At `0d0a33b73` (2026-09-19), `rfc0165_admitted_launch::admitted_string_searches_ordering_find_and_to_int_scan_bytes` improves from a 270-second final-SafeWIR-validation timeout to **1/1 passed in 81.235 seconds** after a separate 47.19-second rebuild, a latency reduction of at least 70% on the identical fixture. The before profile attributed 235/235 samples to `check_memory_cfg -> check_byte_slice -> range_within_for_owner -> less_equal_for_owner/same_scalar`, dominated by rebuilding and sorting `header_length_pairs` inside the owner-bound graph walk. The repair collects that immutable equivalence set once per traversal; a structural regression proves exactly one collection for both an accepted current-owner chain and an unrelated-scalar rejection, preserving fail-closed behavior. The admitted test asserts all five independent expected output lines, and an explicit `witchy parity` run confirms those five lines agree between the canonical SafeWIR interpreter and admitted Wasm. A post-change sample contains no `header_length_pairs` rebuild/sort frames; remaining time is in the next genuine scalar-prefix proof. This closes this exact string-search acceptance blocker, not the broader runtime rows.

At `07ff5f19d` (2026-09-19), after rebasing onto master `cdf957a21`, shallow non-packed aggregate carriers with direct `&Int` fields are admitted through their exact source-derived opaque owner family rather than the checked scalar erasure. The exact rebased-tip shards are **4/4** for the pre-existing `Option(&Int)` lowerer matrix, **2/2** for nominal-plus-tuple execution and its region-root negative, and **2/2** for `Result(&Int, String)` execution and its region-root negative. Independent result 494 for the nominal/tuple program and 7 for the Result program agree between the canonical SafeWIR interpreter and admitted Wasm; all heap audits report zero leaks. Nested aggregates, non-`Int` view payload families, trait evidence, aggregate callable capture, and aggregate `var` write-back remain open at this checkpoint; section 6 deliberately prohibits opt imports of arbitrary normal user modules, so reverse transport is not a positive acceptance row.

At `19cf4931a` (2026-09-19), the exact focused view-witness shard is **3/3 passed**. Trait-mediated `&Int` dispatch and direct `var Holder(&Int)` replacement each produce the independent result 79 in the canonical SafeWIR interpreter and admitted Wasm with zero leaks; the latter proves a field view of the old aggregate remains a snapshot after whole-value replacement. The third witness closes the mode-direction ambiguity: an opt module importing an arbitrary normal user module rejects at the transitive source-mode boundary, as required by section 6, rather than reaching carrier admission. Aggregate callable capture and indirect aggregate-`var` function-value transport remain open.

At `6c5c14db2` (2026-09-19), after rebasing the implementation as `7fd8e7312` onto master `e3535be30`, the stale named-adapter write-back contract blocker is **4/4 passed on the exact integrated tip**: the checker unit accepts only same-layout `ReallocatesParameter -> Unique` tail weakening and rejects the reverse; the structural lowerer witness proves the adapter and owner family bind the post-body promoted target contract; and both the normal relay and normal -> opt -> opt direct/indirect `var unique` programs reach admitted execution. The closure environment's hidden parameter offset is translated symmetrically in lowering, initial owner validation, and source authority. This closes the observed signatures 4/5 mismatch only; aggregate callable view capture and indirect aggregate-`var` transport remain open.

At `409d08d64` (2026-09-19), the list-pop generation-bound blocker is green on the integrated tip. Shared-phi transfer no longer retags an uncarried retained snapshot's bound, guard, or fresh-header evidence merely because a carried `var` lane shares its physical root; only scalar evidence actually transported across the edge is rebound. The new checker witness proves both that the snapshot bound remains current and that it cannot authenticate the rebased phi. That unit passes **1/1**, the exact production `list_pop_loop_writebacks_preserve_unique_and_shared_lanes_on_three_legs` witness passes **1/1**, and the isolated full `rfc0087_uniform_var` shard passes **24/24**. This closes the observed `MissingBoundsGuard { F0, index v2193, count v1299 }` root without relaxing current-generation validation.

The canonical Net adapters now register without grants. Minting and legacy grant adapters remain gated by the corresponding authority. The admitted capability-rights positive case is **1/1**, and the null-token denial matrix for `listen`, `accept`, `close`, and `serve_pool` is **1/1**. These are registration and fail-closed authority witnesses, not proof that the broader capability or installed-workflow rows are complete.

Fail-closed carrier note (2026-09-19): mirrored lowering and source-authority
predicates reject a decoy direct `&Int` field when another field hides a view
through `Option`, a nominal, a callable signature, or a generic argument. The
exact `Mixed(&Int, Option(&Int))` production regression reaches canonical
admission and rejects with `CandidateSignatureMismatch`; the supported
immediate-field program still produces 494 in the canonical interpreter and
admitted Wasm with zero leaks. This is evidence for row G's current partial
boundary, not completion of nested aggregate carriers.

**Exact whole tuple root (row G, 2026-09-24):** The direct `&(Int, String)`
carrier retains the tuple and its String child across a relay, call, return,
and owner replacement. The hand-computed result is
`2 + length("abc") + 9 = 14`;
`rfc0167_tuple_root_view::exact_tuple_view_keeps_both_old_fields_after_owner_replacement`
produces `14` in admitted unoptimized Wasm with `Some(0)` canonical heap leaks.
The same focused target passes 4/4 cases: `&(Int, Bool)` and
`&(Int, &String)` have no authenticated whole-tuple carrier, a region-born
tuple root cannot escape, and closure capture of this tuple view remains
refused. Broader tuple roots and tuple-view captures still need separate
carrier evidence.

## Optimization-readiness carrier audit

This inventory identifies production carriers already capable of preserving the
section 10 preconditions. It is partial evidence for row P, not permission for a
later pass to reconstruct a missing fact. Each row remains open until its
positive and near-miss witnesses cross every relevant source boundary on one
exact tip.

| Source fact | Current checked carrier | Conservative invalidation or current gap |
|---|---|---|
| `own`, last use, `unique` | Lowering `Facts`, `CallOwnershipFact`, and `DropFacts`; type checking separately retains source ownership provenance | Alias/share/escape and call misses kill reuse. No exported summary yet combines last use, nested uniqueness, and destination eligibility through indirect, generic, and trait calls. |
| Immutable view root, projection, retain, and nonescape | Existing `LoanFacts`, `CheckedAccessFacts`, and `CheckedPlaceFacts` retain some roots and projections, but they describe the superseded lexical-loan model | Add one authenticated value carrier whose root/projection survive return, aggregate, generic, trait, callable, indirect, and early-`?` transport. Mutation invalidates in-place permission, not the view; retain/release obligations must join and unwind exactly. |
| `frozen` plus effects | Frozen binding/parameter/field facts in type checking plus checked call and intrinsic effects | Deep writes reject; immutable views preserve their own snapshots. There is not yet a reusable cross-call summary proving the narrower immutable place without inventing purity, totality, or copyability. |
| `var` mutation plus disjoint places | `CheckedPlace` paths and overlap checks; canonical SafeWIR memory/range state | Dynamic overlap and unknown calls lose in-place precision. A live view of the affected root forces copy-on-write unless independence is proved; no mutable-reference carrier may be reconstructed. |
| Concrete type, layout, callable/witness identity | Monomorphized type tables, runtime checked identity, and authenticated canonical call contracts | Missing or mismatched catalog authority rejects. Function values now retain selected qualifiers, but generic, trait, and separate-module parity is incomplete. |
| Bounds/range at an owner generation | Lowering `RangeFact`/`ProvenIndexDomain` and canonical SafeWIR `MemoryState` range, guard, prefix, and generation facts | Stores, moves, consumes, unknown calls, and joins invalidate affected facts. Equivalence between the syntactic lowering facts and canonical checked facts is not yet established across aliases and helpers. |
| Explicit discard plus effects | Type-checker result/discard and `must` obligations plus lowering call/liveness facts | Every effect, trap, termination edge, and affine obligation must survive. No unified summary yet supports dead-result or fusion work through indirect/generic calls. |
| Root provenance plus typed resumable state | `TypedAsyncLoweringInput` and its state, suspension, operation, queue, and source-control bindings, alongside owner/place facts | Invalid region-root escape and terminal identities reject. No single carrier yet combines retained view roots, destination eligibility, cancellation, captures, and replay-free scheduling across all boundaries. |

## Typed-generation cut inventory

This is the current migration boundary for row M. It separates compatibility
tests that intentionally exercise a raw boundary from production generation
that must leave that boundary. A source string is not accepted as authenticated
syntax merely because a caller assembled it from partially typed fragments.

| Raw path | Current disposition | Required sealed replacement |
|---|---|---|
| Fixed string and binary expressions in example fixtures | Migrated | `meta.expr_string` and `meta.expr_binary` now replace the fixed raw-expression callers |
| Comptime `emit(String)` and `console.print` source output | Migrated | The synthetic comptime entry has no `Console` parameter and exposes only typed `emit_item(ItemSyntax)` output; legacy names reject before generated items are merged |
| Glamour tagged-literal holes and interpolation fixtures | Legacy source outside compiler scope | The project still declares `List(String)` tags and calls retired `meta.expr_tag_hole(String)`. The compiler-only typed migration leaves that source untouched; it needs a separate consumer migration before those tags can build against the typed-only ABI. |
| `meta.item(String)`, `meta.expr_raw(String)`, `stmt_raw(String)`, and `block_raw(String)` | Retired | No definition or positive production caller remains. Intentional negative tests reject these names before generated items merge. `derive(Deserialize)` uses structural postfix-try, lambda, and `item_with_imports` builders. |
| Source-backed `ExprSyntax`, `TypeSyntax`, `PatternSyntax`, and `MatchArmSyntax` payloads | Compatibility boundary | Reflection still reparses the wrapped strings. Structural compiler-handle variants exist, but every producer must migrate before the source-backed variants and `parse_*_payload` helpers can be deleted. |
| Source-backed `ParamSyntax`, `StmtSyntax`, and `BlockSyntax` payloads | Compatibility boundary | Reflection still reparses these strings through synthetic function wrappers. Complete the structural parameter, statement, and block builder surface, migrate callers, then delete these decoders. |
| Tagged-expression source holes | Compiler ABI migrated | The compiler supplies `List(meta.TagHole)` and admits only `meta.expr_tag_hole_typed`; active compiler-issued markers become owned expression AST without reparsing hole source. `List(String)` tags reject. The separate legacy Glamour source remains outside this compiler-only boundary. |
| JSON aggregate generation and dynamic constructor calls | Migrated | `meta.expr_tuple`, `meta.expr_call`, and checked identifiers construct the generated function without rendering names or arguments to source |
| Generated ordinary functions and types | Migratable now | Existing typed quote/item/function builders; keep raw positive tests only where they intentionally test the compatibility boundary |
| Generated `gen`, `async`, `region`, `yield`, control flow, and imports | Blocked | Compiler-owned item, statement, and control-flow builders for these shapes |
| Provenance, authentication, and diagnostic negatives for raw syntax | Retained until the cut | Replacement negative tests must pin the sealed boundary before compatibility entry points are deleted |

The cut is complete only when every production caller is structural, the
remaining compatibility tests have an explicit replacement contract, and the
raw entry points are deleted rather than made private aliases for parsing.

## Required small counterexamples

- `take(own view: &String)` followed by a second `take(view)` rejects because
  the carrier binding was consumed; other copied views remain valid.
- `fn first[a](items: &[a]) -> Option(&a)` returns a retained element projection;
  a view of a region-born root cannot escape rewind.
- Taking `let before = &items[0..2]` and then mutating `items[0]` through `var`
  leaves `before` unchanged and detaches storage when required.
- Moving an owned shell with `&String` fields retains their roots; changing an
  unrelated scalar field does not alter the views.
- `own frozen String` transfers; `var frozen String` replaces; field/index
  mutation and every `&mut` spelling reject.
- Normal `snapshot = xs; fast.set_first(xs)` leaves `snapshot` unchanged; opt
  code missing the unique proof rejects before execution.
- Repeat these through labels, imports, function values, generics, traits,
  early `?`, inferred and explicit unit returns, and traps.

## Documentation consolidation completion

The docs task is complete when retired bodies are removed, their exact prior
commit and replacement are indexed, internal links resolve, active evidence is
preserved, and RFC hygiene checks pass. That is distinct from rows A-P above.

## Inherited evidence index (historical, not RFC-0167 acceptance)

The following records are preserved from the retired concurrency/generator
capstones. Their PROVEN labels refer to their recorded commits and original
contracts, not to this RFC or all current canonical compiler paths. Re-run them
before using them to close a row above. Other historical companion ledgers and
machine-readable censuses remain in place for the same reason.

### Historical 0129 evidence

| Row | State | Executable evidence |
| --- | --- | --- |
| 1 | PROVEN | `tests/misc/rfc0129_async_lowering.rs::rfc0129_acceptance_row_1_async_lowering_preserves_source_contracts_on_both_backends` runs mutable loop state, repeated suspension, early return, and a source trap on the interpreter and compiled Wasm. It also inspects the typed suspension-carrier catalog for the exact live `Int` slots and proves the trap retains the source callable and line without leaking generated `__async_` identities. |
| 2 | PROVEN | `tests/misc/rfc0129_channel_contract.rs::rfc0129_acceptance_row_2_typed_bounded_selected_cancelled_and_joined_channels_agree` executes `Int` and `String` channels, capacity-one backpressure, first-ready selection, quiescent closure, cancellation of a parked child, and structured scope/join. It requires the exact 18-line schedule to agree on the interpreter and compiled Wasm. |
| 3 | PROVEN | `tests/rfc0080_0130_conformance/rfc0129_schedule_parity.rs::rfc0129_acceptance_row_3_deterministic_schedule_backends_agree` executes the canonical 13-line schedule from `tests/fixtures/rfc0129_deterministic_schedule.witchy` on compiled Wasm first and then the interpreter. It covers bounded backpressure, quiescent join release, deterministic select order and closure, cancellation, retained-sender quiescence, structured joins, nominal aggregate/list carriers, and a distinct string carrier. Rows 2 and 3 landed together at `5c040cc0` after their focused 99-test coordinator gate passed. |
| 4 | PROVEN | `tests/rfc0129_scalar_executor_performance.rs::million_message_scalar_executor_meets_resumption_cost_and_allocation_gate` executes the real compiled-Wasm producer/consumer benchmark for one million messages, checks its exact fold, proves the scalar entry bypasses `task.run`, requires zero GC task-heap capacity, rejects 100 or more linear allocations, and enforces a 300 ns/message median ceiling. `aggregate_channel_gc_heap_capacity_is_flat` forces the GC-backed scheduler with nominal `Packet(Int, Int)` messages, checks both folds, and pins the same 65,536-byte backing capacity at 100 and 10,000 messages. Together they show that the promoted scalar and aggregate paths do not grow a continuation chain with the message count. |
| 5 | PROVEN | `tests/misc/rfc0129_negative_matrix.rs::rfc0129_acceptance_row_5_rejects_invalid_concurrency_sources_before_lowering` rejects reference escape across suspension, capability transfer through a worker result, dedicated async-trait syntax, and discarded task results. Every case checks source-level diagnostic fragments and proves rejection occurs at parse or the pre-lowering source semantic boundary. |
| 6 | PROVEN | `tests/misc/rfc0129_cooperative_worker_boundary.rs::rfc0129_acceptance_row_6_cooperative_and_parallel_maps_use_distinct_boundaries` executes both APIs on the interpreter and compiled Wasm. It proves that `chan.par_map` keeps its explicitly passed `Console` in one VM with zero worker imports; pure `vm.par_map` crosses the two-phase `vm_par_map_run`/`vm_par_map_write` worker boundary; a capability-capturing callback stays in the parent VM; and `vm.with_dir` grants exactly one explicit `Dir`. `examples/channels/src/cooperative_map.witchy` and `worker_vm_map.witchy` are separate runnable examples, and their README records the distinct API and cost contracts. The exact measurement is the stable zero-versus-two worker-host-interface boundary, not an elapsed-time claim. The merge-coordinator landing at `22968974` passed all 111 selected tests, including this test and the example parity/validation sweeps. |
| 7 | PROVEN | `scripts/installed-bounded-channels-smoke.sh --witchy <binary>` copies the public binary into a fresh install root, resolves only that copy on `PATH`, and uses `env -i` plus fresh home, temporary, cache, and working directories for `check`, `build`, and `run`. It creates the capacity-one `bounded_channels` project and checks its exact deterministic drain output without repository source, standard-library, package, or cache state. `scripts/release-smoke.sh` invokes the same harness on the extracted archive, and `.github/workflows/release.yml` runs it for every supported native release target. The initial installed-workflow slice passed the 584-test coordinator batch landed at `c8ff194a`; the environment-scrub hardening passed the 111-test coordinator gate landed at `22968974`. |

### Historical 0130 evidence

Rows 1-3 are promoted by the owned-frame implementation and its executable
mutation, control-flow, effect-count, frame-shape, and scaling evidence.

| row | status | executable evidence |
| --- | --- | --- |
| 1 mutation and control flow | **PROVEN** | `rfc0130_generator_owned_frame::rfc0130_rows_1_to_3_generators_resume_owned_frames_once_with_linear_work_on_wasm` exercises mutable top-level and inherent generators with loop and branch control on compiled Wasm. `rfc0130_generator_liveness`, `rfc0130_generator_nested_cfg`, `rfc0130_generator_match_cfg`, `rfc0130_generator_match_binding`, and `rfc0130_generator_while_let_cfg` preserve live locals and branch, match, and `while let` control flow across suspension on both backends. |
| 2 one-time body segments and effects | **PROVEN** | The rows 1-3 fixture checks exact pre-yield capability output for every yielded item, including exhaustion. `generator_owned_frame_delays_post_yield_effect_until_resume`, `generator_direct_multi_yield_loop_resumes_each_segment_once`, and the paired CFG fixtures prove that post-yield, multi-phase, conditional, match, and loop effects resume once without replay. |
| 3 linear pulls and bounded frame storage | **PROVEN** | `rfc0130_generator_scaling::rfc0130_acceptance_row_3_owned_frame_pulls_are_linear_and_storage_is_bounded_on_wasm` compares 8 and 64 compiled-Wasm pulls and pins a fixed direct five-lane lazy-entry carrier. The rows 1-3 fixture independently checks fixed carrier dimensions and live cells, deterministic counters, exactly linear effect count, and no pull-dependent RC, bump-allocation, or reown calls. |
| 4 source diagnostics | **PROVEN** | `rfc0130_surface::rfc0130_row_4_generator_failures_name_source_syntax` covers returned values, yielded-value mismatch, borrowed frame/element types, region escape, unsupported trait syntax, and rejection of generated helper names. `generators::target_availability_tests::residual_suspension_cfg_fails_closed_instead_of_replaying` pins the source-located residual-CFG boundary and proves replay is not offered as a fallback. |
| 5 adapters and collection parity | **PROVEN** | `rfc0130_adapter_collection::rfc0130_row_5_iterator_adapters_and_from_iterator_collections_agree_on_both_backends` composes representative lazy adapters, selects `List`, `Dict`, `Set`, and `String` collection through expected-type `FromIterator` dispatch, bounds an infinite adapter pipeline, and short-circuits an infinite `find` identically on the interpreter and compiled Wasm. |
| 6 syntax preservation | **PROVEN** | `rfc0130_surface::rfc0130_row_6_formatter_reflection_docs_and_editor_surfaces_preserve_generators`, `web/witchy-runtime/witchy-highlight.test.mjs`, and the Zed extension's pinned Tree-sitter revision preserve `gen fn`/`yield` through formatting, AST and quoted-item reflection, generated docs, and editor highlighting. |
| 7 clean installed example | **PROVEN** | `scripts/release-smoke.sh`, invoked by the release workflow on every supported native archive, creates a fresh project through the installed public CLI and executes both `finite(4)` and `naturals().take(5)` with an isolated HOME, cache, working directory, and PATH containing only the extracted toolchain. |

## Foundation landing debt (2026-09-20)

Landed with the user's explicit authorization that master may regress before
release. Each row is a test the default gate excludes (`scripts/nightly-deferred-tests.txt`
and the RFC-0089 block of `scripts/rfc0165-deferred-tests.txt`) until the named
track restores it; the tests themselves are unchanged and run under `--full`.

| debt | tests | restoring track |
|---|---|---|
| ~~dictionary values through owned storage stopped admitting / answering~~ **restored 2026-09-20**: type checking now infers fresh-allocator results (`dict.new`, `dict.with_capacity`, `list.with_capacity`, and wrappers) as opt-mode owners, keeps the proof through place assignments and self-updates, and demotes rather than consumes an inferred owner on an ordinary alias (RFC-0167 §4); two snapshot tests' opt legs now pin the required refusal | `rfc0165_dictionary_mutation` ×7, `rfc0165_recursive_generic` ×2, `rfc0165_recursive_render` ×2 — back in the default gate | — |
| ~~corpus `018-…keep_the_prefix` refused (`MissingBoundsGuard`)~~ **fixed and restored 2026-09-21.** The earlier note ("the source's count SSA names an owner generation the inner loop has retired") was close but wrong, and the wrong part mattered: the generation was not retired, the proof was **discarded by the join**. Both predecessors of the copy named the same SSA value as the list's length lane — one at a concrete epoch, one at `JOINED_OWNER_GENERATION` — and the meet kept an entry only when the two generation sets were *equal*, so the sets shared no member and the entry went away while `header_values` survived still naming that lane. Two changes: the meet of generation proofs now keeps the **intersection** rather than requiring equality, and a fresh Length-header observation about a concrete epoch **rebases into the joined epoch** its owner is merging into (on whichever path control arrived, that load was the current length). The rebase is confined to `fresh_header_lengths`: `bound_generations`/`guard_generations` hold derived proofs a predecessor may have established before the other predecessor's mutation, and rebasing those revived a retired guard — `bounds_guards_name_the_current_owner_generation` caught it and pins the refusal. | `rfc0165_corpus`, `rfc0165_admitted_launch::admitted_string_pushes…` — back in the default gate | — |
| ~~ownership row F unoptimized lowering (`UnsupportedIndirectCall`)~~ **fixed and restored 2026-09-21.** One of the four row F cases takes a function VALUE of a callable with a `var` parameter and calls it indirectly. The indirect-call check rejected it because the target's write-back result is `DerivedResult::ReallocatesParameter`, which `is_indirect_result` did not list - although the argument side already admitted `ParameterOwnership::WriteBack` and `result_origin` already mapped that result to `Origin::ReallocatesParameter`. Only the gate was missing. It stays fail-closed: `indirect_target_summary` JOINS every function sharing the signature, and `DerivedResult::join` collapses unequal results to `Unknown`, which `is_indirect_result` still refuses - so a write-back survives only when every candidate callee reallocates the same parameter with the same layout. Verified both ways: two callees of differing shape are refused, two of identical shape admit and run. | `rfc0167_ownership_parity` - back in the default gate | - |
| ~~canonical Net host fixtures~~ **fixed and restored 2026-09-21.** Two of the three were asserting the PRE-canonical boundary: `host::network::link_canonical` is registered unconditionally by design (Wasmtime resolves every import before the admitted entry starts, including one reachable only from a helper `main` never calls), so an ungranted Net program no longer fails to LINK - it fails at the entry, which has no token to mint, before `main` runs and with no output. Those two were migrated to the canonical boundary. The third was a REAL defect and the test was right: neither `connect_at` nor `listen_at` consulted `caps.net_connect`/`caps.net_listen`, so a Connect-only grant could bind a listener. Filed as SEC-097 and fixed by checking each verb's right where the canonical adapter authenticates the token. | `rfc0165_net` x2, `canonical_net_fixture` - back in the default gate | - |
| ~~opt caller of `list.pop` no-copy miss refusal~~ **restored 2026-09-21.** Not a lost refusal: the contract is refused one stage EARLIER than the test expected. Checking now rejects the aliased argument outright ("argument 1 to `helper.take` cannot repair ordinary storage into a `unique` parameter in `mode opt`; pass an existing unique owner"), so the graph never becomes a checked module for `enforce_performance_modes` to judge. The test was migrated to assert the current refusal and still requires it to name the callee, plus a normal-caller control that must still link and repair. | `rfc0165_reference_list_patterns::normal_opt_pop_aliased_opt_caller_refuses_no_copy_miss` — back in the default gate | — |
| ~~async recipe body drive (timeout)~~ **restored 2026-09-21**: it was deferred for a TIMEOUT, not a wrong answer, and the canonical checker is now fast enough to finish. `3ef91ad03` holds the checker's per-program-point facts in sorted runs and `3646362d7` derives a layout's owned-pointer fields once per layout, together cutting this test ~42% (56.6 s -> 32.8 s standalone). It passes in **31.0 s** on `a6768a106`. The other eight foundation-debt rows were re-run at the same tip and still fail on their own merits. | `rfc0077_integration_grants::source_integration_async_recipe_drives_body_instead_of_only_making_a_task` — back in the default gate | — |
| ~~`own unique` FIP entry deferred~~ **fixed and restored 2026-09-21.** Not FIP re-admission — a general language defect the FIP fixtures happened to expose. `c.field = <expr that reads c>` was refused on ANY owned `unique` record: the place-assignment desugar is `c = C(field: <expr>, ..c)`, and `Expr::RecordUpdate` transferred the spread base BEFORE inferring the field expressions, so a field expression reading the same record saw a binding already marked transferred. Written evaluation order evaluates the fields and only then consumes the base, so the transfer moved after the field loop. The affine rule is unchanged: a field expression that itself consumes the base still fails, now in written order. Nine of the ten tests pass unchanged; `owner_escape_is_rejected` was migrated, because `let alias = cursor` on a unique owner is now refused by the checker — the same escape the FIP contract names, caught earlier and by the language rule. | `rfc0089_fip_contract` ×10 — back in the default gate | — |
| ~~retired `View(T,'a)` callable relations~~ **retired 2026-09-21**: `tests/rfc0112.rs` deleted. All FOUR of its tests were deferred (two here, two in the RFC-0165 list) and all four fixtures are written in the grammar RFC-0167 removed — `type Holder('a)`, `input: &'a String`, `fn(let &'supplied String, ...) -> &'supplied String`. They cannot be migrated without asserting semantics the contract deleted, which is the same call row B already made for the RFC-0122 corpora. `crates/witchy-types/src/access.rs` is NOT deleted with them: `access::checked_facts` is live production machinery for codegen assembly, analysis, and specialization. The negative contract stays pinned by `rfc0167_reference_contract_matrix` and by `reference_type_rejects_retired_explicit_lifetimes`, `type_aliases_reject_lifetime_parameters_at_the_declaration`, and `rfc0167_bracketed_function_type_params_reject_lifetimes`; the positive contract is the 18-file `rfc0167_*` view corpus. | `tests/rfc0112.rs` ×4 — removed from both deferred lists | — |
| ~~BUG-639: `chan.select` payload desugar emits a compiler-private intrinsic into user code~~ **fixed and restored 2026-09-21.** A `select` whose match is a VALUE (not just side-effecting arms) makes the desugar emit `chan.selectN_frame_payload`/`_decode` into the USER's module, and the scheduler-bridge privacy rule rejected the compiler's own output. That rule exists to stop SOURCE from forging scheduler authority, so it now applies to source-written calls only: a desugar tags what it generates with the `@` prefix, which source cannot produce (`@` lexes as `Tok::At` and never begins an identifier), and the linker strips the tag while skipping only the privacy gate. `MESSAGE_BRIDGE_CALLERS` is unchanged. The census also needed two book blocks migrated to `let _ =` — RFC-0167 row C removed the non-unit `var`-call exemption and their prose still described it. | `rfc0087_migration_census` — back in the default gate | — |
| ~~opt mode refuses `dict.new()` as a fresh `unique` owner~~ **restored 2026-09-20** (same change) | `lsp::tests::mode_opt_no_copy_violation_names_the_alias_reason` — back in the default gate | — |

# Bounded Vec caller proofs

The generic `vec_push_spare<T>` body remains a single universal theorem over an opaque element type, a focused allocation and an unrelated heap frame. Its authored prefix requirement is an assumption of that body theorem. Callers now produce separate `call_precondition` goals proving the complete supported requirement at each actual primary typed call site. Independent source/control reviews and focused Rust/real-Lean/Wasm qualification exercise the bounded source slice; the physical compiler/runtime correspondence remains trusted.

The shared finite typed evaluator tracks allocation IDs with actual element type, key, requested capacity, cells and provenance. Actual `memory.try_alloc` evaluates its checked allocator permission and capacity, then branches on an arbitrary success discriminator. Normal Err creates no owner; normal Ok creates a fresh logical focus with the requested capacity and all-None cells. Allocation success is never assumed. Actual constructors, field updates, moves, Copy arguments and finite branches determine the caller prefix. Arbitrary entry records have independent bounded headers and valid cells but no assumed prefix. Source Flow remains the sole availability, borrowing and MUST checker.

Caller occupancy uses an arbitrary opaque alpha, including for concrete scalar and padded-record layouts. Caller Init supports typed nontrapping Copy reads and scalar literals, abstracted by universally quantified alpha values, and uses the same bounds/Empty/update transition as the generic body. Caller requirements initially admit Prefix, Empty, U64 comparisons and boolean connectives. Requirements inspecting element content, allocation equality or authored history fail closed rather than being erased. All actual call arguments are evaluated in runtime order, including arguments ignored by a payload-insensitive requirement.

A stable ordinal is attached once to every contracted primary typed call after its actual arguments. Init, record and U64 collectors share it. The record inventory includes unused, generic, nested, branch and unreachable occurrences, and combines every symbolic path into one goal per site. Literal-dead sites receive an explicit False-implies-False obligation. Unknown or unverified helper calls before a needed site refuse explicitly, including mutable scalar effects before fresh allocation. Without an eligible owned-post summary, record calls invalidate returned heap/result state and authored postconditions are not reused. Sequential use of such an unknown result before another required site remains unsupported; the bounded owned-post slices below extend eligible Result continuations. Fresh modeled allocation can introduce a new focus after a checked record call, while old focus history remains unknown. Structural suffix handling carries an explicit needed-after continuation through statements, conditions, subexpressions and argument lists; an outer ordinary call after its last nested record argument is permitted only when no later record site remains. Unverified helpers before later arguments, outer record preconditions, branch sites or following statements still refuse. Returned expressions have no following-statement continuation. This is normal-reaching-call partial correctness; unknown results never acquire implicit callee facts. Suffix handling stops only after the last record site, considering branch continuations; cleanup and printing then remain independently source checked. The independent Init inventory still visits Init-wrapper calls in that suffix.

Limits are 4096 nodes/states/expanded terms and 64 KiB total expected statements per caller; existing contract depth and clause limits remain. Loops and unsupported effects before required sites refuse. A caller inside another contracted body and mixed U64 caller contexts outside the existing scalar fragment also refuse explicitly. These limits do not activate growth, loops, owner elements, zero-sized storage or the default Vec/String libraries.

The old body/Init/scalar model identities remain unchanged when no record caller goals exist. Caller goals append `witchy.checked-storage-record-caller-allocation.v1` to the fresh model identity and retain `witchy.checked-storage-record.v1` source-goal metadata. The existing one proof batch, expected-type/theorem/dependency/axiom audits, tool checks, currentness binding and atomic publication remain unchanged; there is no admission reuse. The caller theorem establishes logical requirements on normal reaching-call paths. Physical allocator freshness and identity, host grant correspondence, metadata/bitmap/count/codec mapping, Wasm emission and VC translation remain compiler/runtime TCB. It does not establish allocator success, whole-main termination or absence of traps outside those paths.

Qualification exercises the unchanged extracted library body, four real layouts, direct and generic callers, bad headers and occupied tails, repeated and dead sites, source/ownership failures, proof-erased byte equality, fresh artifact binding and old-mode compatibility. The body theorem is conditional on authored requires; callers establish those requires from actual state. The ordinary full Vec/String libraries are not claimed as fully functionally proved.

The qualified owned-post control slice is independently selectable in
`scripts/test-wasm-vec-proof.mjs` (a Node suite, removed 2026-11-16 with Node; docs/decisions.md): `ordinary owned post two-push` executes twelve
real generic calls, and `real owned post two-push prefix` selects the smallest
native positive. It keeps the exact extracted push body, one generic body theorem
and two actual caller-site goals. A generic `exercise_twice<T>` allocates and
constructs a fresh Vec, then invokes the second push only inside the first Ok
arm. All printing/cleanup happens after the final required site on that path.
Four emitted layouts each execute capacities0/1/2 with distinct first/second
items, covering first Full, second Full and two successful appends. Headers and
both read occupancies are consumed; retained emitted-function diagnostics cover
exact payloads/items/bitmap/padding/frame/cleanup. The proof-erased complete
program must produce identical Wasm bytes. Focused qualification passed the combined compiler candidate and these controls;
the exact scope and evidence are recorded below.


The owned-post slice uses the compiler's modular VC rule: the complete
callee body obligation and every predecessor call precondition must be present
and freshly checked in the same atomic batch before their consequences justify
a later caller obligation. This is an explicit dependency/translation rule in
the compiler TCB, not a kernel-linked executable refinement theorem or a new
admission/cache family. The conditional model tag is
`witchy.checked-owned-record-post.v1`, appended only when owned posts are used;
existing per-goal `witchy.checked-storage-record.v1` metadata remains unchanged.
Root independently qualified the combined candidate after the control author
released it.

An eligible descriptor is sealed from actual typed finite returns and modeled
effects preserving the original sole owner/allocation. Authored Same/Frame
clauses cannot manufacture that seal. Actual call arguments are evaluated before
an immutable call-entry snapshot is captured. Only whole supported Prefix/U64
conjuncts from the selected Ok arm are projected over fresh typed public fields
and after-cells; unsupported conjuncts may be omitted soundly, but expressions
inside disjunction/negation are not erased. Full authored ensures, including
payload and frame clauses, still require the universal body proof. A missing
Ok Prefix cannot be inferred merely from the seal. Mutating a returned header
or initializing its tail affects the current state without rewriting old/after
snapshot facts. Err-arm state remains unknown for another proved call; ordinary
terminal cleanup is independently source checked. Source Flow remains the
ownership/borrowing/availability authority.

Further focused selectors are `owned post unsupported` (Err reuse, unknown
helper and duplicate/moved-owner refusal before Lean), `real owned post negatives`
(actual kernel failures for first-pre, second header/length/tail, rewound length,
omitted/false Prefix and invalid functional post), and `real owned post renaming`
(renamed definitions/fields plus fresh executing artifact controls). The first
native selector `real owned post two-push prefix` contains only the core positive,
inventory, meaningful byte erasure and runtime. Run it before expanding the
frozen affected matrix; ordinary execution is separately selectable.

Artifact omission/tampering controls remove the body or predecessor precondition,
change the second expected statement, or corrupt the source header/tail. Fresh
checking must reject self-rehashed evidence and preserve existing publication.
These are consumer/complete-batch controls. Private dependency-graph missing,
cyclic, future and self-edge validation requires the production worker's focused
Rust tests; a self-edited artifact is not direct coverage of that private graph.
No unsupported control is silently counted as an actual kernel negative. Existing
old helper/loop/T-content barriers remain; no blanket sequential-call refusal
was present in the previous Node controls to retire. Physical allocator/codec/
emission correspondence, whole-program termination, allocation success and the
full growth/String/owner-element libraries remain outside these logical claims.


Combined focused qualification passed five selected Node groups: the actual
two-push positive, the existing one-call positive, static ownership/history
refusals, eight real semantic negative programs, and renamed/fresh artifact
controls. The batch ran for 122.095 s; it is correctness evidence, not an edit
latency measurement. An earlier affected Rust selection passed 16 tests and the
ordinary twelve-call runtime/payload/cleanup control passed separately. Two
initial proof-generator failures were preserved and diagnosed from complete
generated Lean output: unused shared-term simplification, then opposite-direction
path-equality rewrites. The bounded repair preserves propositions and proof
limits and uses local assumption/simplification for actual owned-post paths.
Independent source and repair reviews found no blocker.

Exact candidate e20932735a20e554d5344f2c19aad6d3d54ce9c6 and compiler SHA256
`b4b4c46d5496fcd8bfebf1da414f2b9fe64c4134bdda5f16cc8ff2d175465377`
are recorded in `generic/witchy-wasm-coordinator-94/owned-post-qualified-839`.
All source, Main preservation, pilot and owned-child guards passed. The final
integration differs from that checked candidate only by documentation.
This remains a finite, same-owner, first-Ok composition slice; it does not prove
whole Vec growth, loops, owner elements, String functionality or physical backend
refinement. Err continuation was unsupported at that first milestone; the later slice below extends it.

The next both-arm candidate adds an authored Prefix of the actual returned Err
owner to the unchanged generic push body contract. Controls select
`ordinary owned arms retry` for erased runtime and FIRST NATIVE
`real owned arms returned Prefix` for the minimal proof. The generic caller now
retries from the first Err as well as calling again from first Ok: one universal
body and three actual caller-site goals are reused across four layouts and
capacities0/1/2. The twelve exercise invocations make twenty-four push attempts;
capacity0 now executes first Full and retry Full, capacity1 second Full, and
capacity2 both successful appends. Distinct items, headers and occupancy outputs,
cleanup and meaningful proof-erased byte equality remain consumed. Root qualified this separate extension with the checks recorded below; it
does not retroactively expand the earlier839 scope.

Both-arm eligibility is explicit: the projected Prefix must name the selected
returned arm's exact typed sealed Buffer-owner path, with `before=false`.
`old(input.data)` Prefix, the other arm's facts, header equalities, Same/Frame,
Unchanged, true, negation and disjunction cannot create that eligibility. Full
selected-arm body obligations still prove every authored clause. The conditional model is `witchy.checked-owned-record-post.v2`; source-goal
record.v1 metadata and the complete-batch dependency rule remain unchanged.
Fresh fields/cells remain demonic, entry snapshots follow all arguments, and
Source Flow continues to authorize moves and borrowing. Old Err callers without
an eligible returned Prefix retain their static unknown-history barrier.

Additional selectors are `owned arms require explicit` for strict static
missing/old/Unchanged/true/Not/Or Prefix, unknown-helper and moved-owner controls,
and `real owned arms negatives` for actual kernel refusals and renaming.
The native cases retain a positive-form eligible Err Prefix while making its
claim false, corrupt actual Err return headers, check that Err facts cannot
repair missing Ok facts, and mutate the retry header to challenge old/after
snapshot separation. The existing complete-body/predecessor/dependency and
artifact/currentness matrix from839 remains relevant; no duplicate artifact
campaign is added because no artifact format or binding rule changes.

For qualification evidence, each invoke/refuses helper now retains compiler
stdout/stderr beside the existing per-case source/module stem, including successful
receipt JSON. Current assertions and process/output/time limits are unchanged; the
performance harness is unaffected. Root can inspect actual fresh/kernel diagnostics for every selected
negative without repeating proof checks or relying on observing Node descendants.
The control author ran no checks; Root independently checked the combined candidate.


Both-arm qualification passed the first native positive and six affected semantic
unit controls in843. Its fresh receipt contains one universal body and sites0/1/2
(9,239 statement bytes, 21 emitted function instances); the erased program has
zero obligations and identical runtime bytes. Four further focused groups passed
in844 (47.679 s) using the same pinned compiler without a Rust rebuild: retained
Ok composition, the old Err barrier, explicit active-arm gates, and new native
negatives/renaming. All source, Main preservation, pilot and child-reap guards
passed. Candidate0ad30404b330e360bedb16dd1a7805ba4bef1c79 and compiler SHA256
`23fc55fa8629b4b28ac4bf573d62ad45895425847e3615575c31d10d9200df18`
are pinned under `generic/witchy-wasm-coordinator-94/owned-arms-first-843` and
`owned-arms-controls-844`. Independent source/control review found no blocker.

Retained diagnostics distinguish three intended semantic negatives (false Err
Prefix, wrong actual Err header, and missing Ok Prefix) from the extra retry-header
case. The latter failed closed at a simplifier recursion limit; it is not qualified
as semantic snapshot-negative coverage. That automation limitation is retained
explicitly rather than repaired through higher limits or weaker obligations.
This remains bounded same-owner finite composition, with all prior full-Vec,
loop, String-functional, owner-element and physical-refinement limitations.

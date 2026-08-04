---
rfc: 0108
title: "Glamour stateful browser ABI and checked patch protocol"
status: accepted
created: 2026-07-29
updated: 2026-08-03
superseded-by:
tracking: "Design accepted; no open questions. BLOCKED-EXTERNAL (2026-08-04 triage): the two open Phase-3 rows both need out-of-sandbox evidence — a release-channel Chromium/Firefox/WebKit browser matrix in release CI (ACCEPTANCE.md:121) and a controlled release-host timing report on pinned macOS-arm64 + Linux-x86-64 machines (:123). No in-sandbox code remains; the protocol, ABI, and codecs are implemented and proven. RFC-0107 Phase 3, implemented and proven: binary patch protocol, Wasm-resident model, and typed completion codecs (web/witchy-runtime/glamour-optimized.mjs + glamour-protocol.mjs); Phase 3 rows PROVEN in projects/glamour/RFC-0107-ACCEPTANCE.md. Remaining before implemented: retire the compatibility JSON path (Phase 7)."
predecessors:
  - "[0007](0007-rune-runtime.md) (pure-compute Wasm host and data marshaling)"
  - "[0008](0008-frontend-framework-rune.md) (Glamour reference application loop)"
  - "[0107](0107-glamour-next-generation-web-framework.md) (Glamour 1.0 delivery contract)"
---

# RFC-0108: Glamour stateful browser ABI and checked patch protocol

## Summary

Glamour browser applications receive a dedicated stateful Wasm ABI. One Wasm
instance owns one mounted application, including its typed model, authorization
value, previous template-slot snapshot, command state, and subscription state.
The browser sends bounded binary event frames and receives bounded binary
mount, patch, effect, and subscription frames. It never receives the model or a
complete VNode in the production path.

The compiler recognizes a checked family of ordinary Witchy functions generated
for a Glamour `Program`. It synthesizes narrow Wasm exports around those
functions, owns the persistent state root, and keeps Witchy's internal value
layout private. The JavaScript host validates a stable public frame format
before it mutates the DOM or performs an effect.

The JSON protocol remains the readable compatibility implementation and
differential oracle. It is not a fallback after an optimized application has
mounted: a malformed optimized frame fails closed and disposes that
application.

## Motivation

The existing `String -> String` export deliberately restores the guest heap to
its pristine base after every call. That is correct for pure one-shot
functions, but it means a Glamour model cannot remain in Wasm. The current
Glamour host therefore retains the model as JavaScript JSON, sends it into Wasm
on every event, receives it again with a complete VNode, and performs a general
host-side tree diff.

RFC-0107 requires the opposite production architecture:

- one application instance owns its model in Wasm;
- events identify declarative decoder plans instead of carrying executable
  handlers or complete messages;
- static template plans mount once;
- later dispatches carry only changed slots and structural region operations;
- buffers have explicit bounds, sequence numbers, and lifetimes;
- the host validates application output rather than trusting it.

Using the internal boxed-value layout as a public ABI would make compiler
optimizations breaking changes. Letting application code import DOM functions
would widen authority and make validation inseparable from every call. A small
compiler-synthesized state boundary preserves both encapsulation and the
existing capability model.

## Design

### Source contract

The maintained web build generates four ordinary browser-target Witchy
functions from a `Program`:

```
@browser
pub fn glamour_init(root: UiRoot, input: Bytes) -> BrowserState

@browser
pub fn glamour_dispatch(state: BrowserState, input: Bytes) -> BrowserState

@browser
pub fn glamour_emit(state: BrowserState) -> Bytes

@browser
pub fn glamour_release(own state: BrowserState)
```

`BrowserState` is a private nominal type generated in the application module.
It contains the concrete authorization, model, message-decoder table, previous
slot snapshot, pending effects, subscriptions, and independent next input and
output sequence numbers. It
cannot contain a bare grantable capability that was not derived from the
`UiRoot` supplied to `glamour_init`.

`UiRoot` is supplied from the build-authenticated mount grant, not decoded from
application bytes. The host binds that sealed grant to one Wasm instance before
calling init or resume; the compiler wrapper rejects an absent, crossed-instance,
or wrong-build binding before application code runs. Content-addressed compiled
modules may be shared, but each mounted application or island has a distinct
stateful instance and grant binding. No frame contains a capability token.

These declarations are implementation adapters, not application authoring
boilerplate. Phase 3 examples may check them into generated fixtures. The
integrated build in RFC-0107 Phase 4 generates them from the application's
ordinary `Program` declaration.

The compiler accepts the family only when:

- all four functions exist in one module;
- the functions are public, non-generic, non-async, and `@browser`;
- init and dispatch return the same private nominal state type;
- dispatch and emit accept exactly that state type;
- release accepts that state with the `own` convention;
- input and output use `Bytes`;
- the functions' inferred capability footprint is empty after the compiler
  supplies the sealed `UiRoot`;
- no other public function exposes the private state type.

A partial or mismatched family is a compile error at the first declaration,
with notes on the missing or conflicting members.

### Wasm exports

For an accepted family, the compiler emits:

```
__glamour_protocol_version() -> i32
__glamour_input_reserve(length: i32) -> i32
__glamour_init(input_ptr: i32, input_length: i32) -> i32
__glamour_resume(input_ptr: i32, input_length: i32) -> i32
__glamour_dispatch(input_ptr: i32, input_length: i32) -> i32
__glamour_output_length() -> i32
__glamour_output_release() -> ()
__glamour_dispose() -> ()
```

The result of init or dispatch is the pointer to the first output byte.
`__glamour_output_length` reports its length. No exported pointer identifies
the model or the compiler's boxed values.

Resume consumes a checked start frame whose start-specific flag bit zero is set,
installs the same private model state as init plus inert startup work, and
returns zero with a zero output length. It never calls the emit adapter. The
host may use it only after authenticating and adopting the existing static DOM.

The host then performs exactly one input-sequenced activation dispatch. When an
authenticated browser event promoted activation, that ordinary event frame is
the activation dispatch. Otherwise the host sends an empty activation-commit
frame. The guest reconciles startup commands and subscriptions with the event's
transition, commits every surviving private callback entry, and emits the first
post-resume frame at the adopted output sequence. No startup work becomes host-
visible before that frame is accepted, and the existing DOM is not replayed.

One Wasm instance has exactly one state slot. Init and resume trap if a live
state already exists. Dispatch traps before either initializer, during another
dispatch, or after dispose.
Dispose is idempotent from the host's perspective and consumes the state by
calling the generated release adapter exactly once.

The host reserves the input buffer, copies one complete event or start frame,
and calls init or dispatch. The compiler wrapper bounds-checks the pointer and
length again before constructing `Bytes`. A call consumes the input frame.

Emit returns an immutable output frame. The host validates and applies or copies
every needed payload before calling `__glamour_output_release`. Dispatch cannot
begin while an output frame is borrowed. Release invalidates its pointer.

### Memory ownership and dispatch arenas

Persistent application state and per-dispatch storage are separate ownership
domains.

- The compiler-owned state slot is the only persistent root.
- Dispatch borrows the old root, constructs the next state, installs the next
  root atomically, and then releases the old root.
- A failed update, view, encoding, or bounds check leaves the old root installed
  and produces no applicable frame.
- Input and output arenas have one live allocation each and grow
  geometrically up to manifest limits.
- Releasing an output resets its arena. Starting the next dispatch resets
  transient input and evaluation storage after the next persistent root has
  been copied out.
- The host cannot retain a guest-memory view across a call, release, or memory
  growth. Payloads needed by asynchronous effects are copied into bounded host
  values during validation.

The implementation reports current and high-water bytes for persistent state,
dispatch storage, input, and output independently. Repeated bounded dispatches
must reach a stable memory high-water mark.

### Common frame header

Every input and output frame begins with this 48-byte little-endian header:

| Offset | Width | Field |
| --- | ---: | --- |
| 0 | 4 | ASCII magic `GLMR` |
| 4 | 2 | protocol major |
| 6 | 2 | protocol minor |
| 8 | 1 | frame kind |
| 9 | 1 | flags; unknown bits are invalid |
| 10 | 2 | header length, initially `48` |
| 12 | 4 | total byte length |
| 16 | 4 | operation count |
| 20 | 4 | application identity |
| 24 | 8 | build identity prefix |
| 32 | 8 | monotonically increasing sequence number |
| 40 | 4 | string/byte table offset, or zero |
| 44 | 4 | optional development trace offset, or zero |

The build manifest binds the complete build digest; the header prefix rejects
accidental cross-build frames cheaply. Application identity is a compiler
assigned manifest index, not an author string.

Frame kinds are:

| Value | Direction | Meaning |
| ---: | --- | --- |
| 1 | host to Wasm | start |
| 2 | host to Wasm | delegated browser event |
| 3 | host to Wasm | effect completion |
| 6 | host to Wasm | resumed activation commit without an application event |
| 16 | Wasm to host | initial mount |
| 17 | Wasm to host | DOM patch |
| 18 | Wasm to host | effects and subscriptions only |
| 31 | Wasm to host | development diagnostic |

Major versions are incompatible. A host may accept a newer minor version only
when every flag and operation is known and the header length lets it skip
explicitly optional fields. Unknown frame kinds, flags, or operations fail
closed.

All offsets and lengths are unsigned and relative to the start of the frame.
Ranges must be inside `total byte length`, must not overflow 32-bit arithmetic,
and may not overlap the fixed header or operation table unless the field
explicitly names that table. UTF-8 fields require strict decoding; replacement
decoding is forbidden.

Sequence numbers have two independent domains. Host-to-Wasm dispatch frames
(events, completions, action lifecycle inputs, and activation commits) use the exact next input
sequence, beginning at the authenticated resume plan's `input_sequence` or zero.
Wasm-to-host frames use the exact next output sequence. A fresh initial mount is
output sequence zero; resumption adopts the static render's authenticated next
output sequence without emitting a frame. The start frame itself uses sequence
zero and initializes rather than consumes the dispatch-input domain.

Every successful dispatch emits exactly one output frame, including an empty
effects frame when the accepted input produces no DOM or host work. The guest
advances its input counter only after accepting the complete input, and the host
advances its output counter only after validating and atomically accepting the
complete output. A malformed or wrong-sequence input fails the application
boundary; counters are never inferred from model changes, patch count, or one
another.

An activation-commit frame is exactly the 48-byte common header with kind `6`,
zero flags, zero operations, and no payload or trace offset. It is valid only as
the first dispatch after a resume-flagged start frame. A duplicate activation
commit or a completion before activation commit fails closed. A real activating
event replaces this empty frame; the host never sends both.

### Event frames

A delegated event frame contains:

- the stable event-plan ID;
- the template-instance ID that owns the plan;
- the event class ID;
- allowlisted field bits;
- length-delimited `value`, `key`, and selected form-control values;
- boolean `checked`;
- trusted-host status bits for composition, autofill, and user activation.

The manifest caps every field and the complete frame. No DOM object, node
pointer, file, clipboard object, arbitrary event property, or secret value
crosses the boundary. Secret controls emit only the declared non-sensitive
status.

The Wasm runtime confirms that the plan belongs to the live template instance
and event class. It applies `prevent_default` and `stop_propagation` decisions
from the already-mounted plan. The host may perform those two synchronous
actions before dispatch only after resolving the same authenticated plan
metadata.

Effect completions use compiler-assigned effect instance IDs and a result tag
allowed by that effect descriptor. Stale, canceled, duplicate, and
wrong-sequence completions are inert and observable in development diagnostics.

### Mount and patch operations

Operations use an 8-byte prefix: `u16 tag`, `u16 flags`, and `u32 record_length`.
`record_length` includes the prefix and is at least eight. Unknown flags,
undersized records, records extending beyond the operation area, and a decoded
count different from the header count are invalid.

Version 1 defines:

| Tag | Operation |
| ---: | --- |
| 1 | `Mount(template, instance, parent_region, before, slots)` |
| 2 | `SetText(node, value)` |
| 3 | `SetProperty(node, property, value)` |
| 4 | `SetAttribute(node, attribute, value)` |
| 5 | `RemoveAttribute(node, attribute)` |
| 6 | `SetBooleanAttribute(node, attribute, enabled)` |
| 7 | `EnterBranch(region, template, instance, slots)` |
| 8 | `LeaveBranch(region)` |
| 9 | `ListInsert(region, key, before_key, template, instance, slots)` |
| 10 | `ListMove(region, key, before_key)` |
| 11 | `ListRemove(region, key)` |
| 12 | `MountChild(region, template, instance, slots)` |
| 13 | `UnmountChild(region)` |
| 14 | `SetEventPlan(node, event_class, event_plan)` |
| 15 | `RemoveEventPlan(node, event_class)` |
| 16 | `SetClassList(node, value)` |
| 17 | `SetAria(node, attribute, value)` |
| 18 | `SetCustomProperty(node, custom_property, value)` |
| 19 | `ListInsertDynamic(region, key, before_key, template, slots)` |
| 20 | `ListMoveDynamic(region, key, before_key)` |
| 21 | `ListRemoveDynamic(region, key)` |
| 22 | `UpdateDynamicSlots(region, key, slots)` |

Node, region, property, attribute, custom-property, event, and template IDs are
manifest-table indices. They are never free-form sink names. Strings and bytes
are referenced by checked offset/length pairs into the payload table.

Protocol minor 1 extends `EnterBranch` with the slot-count word already present
on `Mount`, `ListInsert`, and `MountChild`, and permits structural slot records.
Each record contains a nonzero unique slot ID plus a checked UTF-8 payload
reference. A minor-1 structural mount must cover the authenticated template slot
table exactly. Minor-0 `EnterBranch` retains its shorter record and minor-0
`ListInsert` and `MountChild` continue to require a zero slot count.
Protocol minor 2 adds `SetCustomProperty`. Its numeric sink resolves only to a
manifest-authenticated `--glamour-*` name and color, length, or number category.
The host revalidates the canonical category token and applies it only through
`CSSStyleDeclaration.setProperty`. Older frame minors reject the tag.

Protocol minor 3 adds arbitrary keys to compiler-declared homogeneous list
regions. Each such region names one authenticated dynamic template whose event
and nested-region tables are empty. Dynamic operation keys are bounded UTF-8
application data used only as keys in the region's private map; they are never
DOM names, selectors, markup, or global node identities. Empty strings are
invalid keys and a zero-length `before_key` means append. The decoder copies and
validates every key before planning mutations.

Cloned template node IDs remain local to one dynamic entry. They never enter
the application's global node table, so two instances may safely share the
same compiler-local template IDs. `UpdateDynamicSlots` resolves the live entry,
requires exact coverage of that template's authenticated slot table, validates
every sink and value, and queues the resulting writes for the frame's atomic
commit. Dynamic insert, move, removal, and slot update reject unknown regions,
wrong templates, duplicate keys, missing `before_key` entries, event-bearing
templates, nested regions, and non-dynamic entries.

Compiler-generated stateful modules advertise protocol 1.3; host-to-Wasm start,
event, and effect-completion frames retain their 1.0 layout. A host rejects an
output frame whose minor version exceeds either host support or the module's
declared version.

The host validates the entire frame into an inert operation list before applying
its first operation. Validation confirms:

- the build and application identities;
- exact next sequence number;
- operation and payload bounds;
- manifest membership and operation compatibility for every ID;
- that node and region identities are live under the named application root;
- unique template-instance IDs;
- unique keys within one keyed region;
- URL category and normalized scheme for URL payloads;
- maximum operation count, payload size, nesting depth, and list length.

Applying a validated frame must not invoke application code. If an unexpected
DOM exception occurs, the host disposes the application rather than applying
the remainder and pretending the sequence succeeded.

### Static templates and changed slots

The build manifest contains each template's static construction program, slot
types, event plans, regions, source mapping, and accessibility metadata. Initial
mount names a template and supplies its dynamic slots. The host constructs or
clones only manifest-authenticated static DOM.

Wasm retains the previous value of each dynamic slot. After update it evaluates
the pure view, compares slots with type-specialized equality, and emits nothing
for unchanged slots. Static structure is not re-sent.

For a template-compatible update, one changed text slot produces one
`SetText`. It does not produce `Mount`, sibling operations, unchanged
attributes, event-plan replacement, or a complete subtree.

Typed custom-property assignments retain one compiler-owned sink per property.
Initial and structural template slots fill only those authenticated sinks;
later changes emit `SetCustomProperty` without accepting a generic `style`
string or a free-form property name.

Dynamic structure uses explicit branch, child, and list regions. A shape that
cannot use a checked plan may enter the documented reference fallback only
before optimized mount; it cannot inject a VNode JSON operation into an
optimized frame.

### Delegated events

The host installs at most one listener for each event class and application
root, using capture only where the browser event requires it. Nodes carry
numeric event-plan metadata, not closures or per-node listeners.

The listener walks the composed path no farther than the application root,
selects the nearest live plan for that class, extracts only its declared fields,
performs authenticated propagation/default actions, and dispatches one event
frame. Removing or replacing a plan updates metadata; it does not add another
root listener.

Focus, blur, mouse enter/leave, pointer capture, composition, and other events
whose native propagation differs receive explicit event classes with tested
delegation behavior. The runtime does not assume every event bubbles.

### Keyed regions

Keys are application data scoped to one live list region. The Wasm runtime
rejects duplicate keys before emitting a patch.

The first implementation computes the longest increasing subsequence of retained
old positions. Retained entries in that subsequence stay in place; other
retained entries emit `ListMove`; new entries emit `ListInsert`; absent entries
emit `ListRemove`. Operations preserve the DOM nodes for retained keys.

For a compiler-declared homogeneous region, the manifest also binds one
event-free, nested-region-free template as its dynamic prototype. Initial
entries are adopted under their source keys. Later source keys use the
minor-3 dynamic operations and remain scoped to that one region; a key in one
region cannot name an entry in another. Retained dynamic entries receive exact
slot updates without remounting, preserving their DOM-owned state.

The host snapshots and restores selection only when a browser operation would
otherwise disturb it. Retained nodes preserve focus, selection, IME composition,
autofill state, media state, nested scroll, and host-owned secret controls.

### Effects and subscriptions

Effect and subscription records share the checked frame envelope but have
separate operation tags and manifest tables. Authority tokens never enter the
frame. The Wasm state names a capability-derived descriptor; the host resolves
that descriptor against the authority granted at mount.

Protocol major 1 reserves the following effect-operation tags:

| Tag | Operation |
| ---: | --- |
| 256 | `StartEffect(instance, cancellation_key, descriptor, request)` |
| 257 | `CancelEffect(cancellation_key)` |
| 258 | `SyncSubscription(subscription, descriptor, request)` |
| 259 | `RemoveSubscription(subscription)` |

Zero is reserved for “no cancellation key” on `StartEffect` and is invalid for
explicit cancellation and subscription identities. Descriptor IDs index
build-authenticated effect or subscription tables. Request strings are copied
from checked payload ranges before Wasm output is released. They contain
application data, never authority tokens or host secrets.

Every async effect receives an instance ID and optional cancellation key.
Completion must match the live instance, expected result schema, application,
build, and sequence policy. Subscription reconciliation uses stable IDs and
cancels removed sources before the next frame becomes observable.

The host assigns a monotonically increasing nonzero generation whenever it
starts or replaces work. An effect or subscription completion input contains
source class, instance or subscription ID, generation, descriptor ID, result
schema ID, success/error status, and a checked payload range. A late callback is
dropped in the host unless all live identity fields match. Witchy independently
validates the application, build, frame sequence, source class, pending instance
or subscription, descriptor, result schema, and callback entry before
constructing the typed message. Because protocol 1 does not acknowledge the
host-assigned generation when work starts, Witchy validates that the completion
generation is nonzero and records it for diagnostics but does not claim a second
generation-liveness check. The pending guest entry still makes duplicate effect
completions and callbacks after local cancellation inert. A protocol that wants
independent generation-liveness validation on both sides must put a
guest-selected generation in the start/sync records or add a start
acknowledgement.

An effect completion retires its host and guest entries exactly once. A
subscription emission leaves both entries live; replacement, removal, or
application disposal retires them and their callback environment.

The completion payload range is schema-bound bytes. A production descriptor
selects one closed host encoder and Witchy decoder, both of which enforce the
same field, nesting, and byte limits. Text fields use strict UTF-8, while other
fields retain their canonical binary representation. The generic UTF-8
`String(value)` encoder used by the protocol floor is a test/compatibility
adapter and does not satisfy an optimized production result schema.

Protocol 1 pins the standard completion payload codecs. All integers are
unsigned little-endian. Reserved bytes must be zero, lengths must consume the
payload exactly, and every text field must be strict UTF-8.

| Result schema | `Ok` payload | `Error` payload |
| --- | --- | --- |
| unit timer/interval | empty | invalid; host setup failure aborts the application boundary |
| HTTP | `01 00 00 00`, `status:u32`, `body_len:u32`, `body` | `02 00 00 00`, `problem_len:u32`, `problem` |
| navigation | `01 00 00 00`, `path_len:u32`, `path` | `02 00 00 00`, `problem_len:u32`, `problem` |
| port/secret | `01 00 00 00`, `value_len:u32`, `value` | `02 00 00 00`, `problem_len:u32`, `problem` |

An HTTP success status is in `100..=599`. A schema/status/variant mismatch,
non-canonical length, nonzero reserved byte, invalid UTF-8, or trailing byte
fails before callback invocation. Witchy first resolves the pending identity and
its build-authenticated descriptor, then selects the closed decoder from that
descriptor, validates the complete payload, and only then loads and invokes the
callback entry. The descriptor's authenticated semantic category selects the
host encoder; neither the result schema number nor a runtime string selects
executable codec code. Publication rejects a production descriptor whose
semantic category has no closed codec.

`SyncSubscription` is idempotent when descriptor and copied request are
unchanged. A changed synchronization cancels the old generation before starting
the replacement. `RemoveSubscription` cancels before the accepted frame becomes
observable. Duplicate effect instances or subscription identities in one frame
are malformed.

The host copies effect request values that outlive output release. It applies
DOM operations before starting newly emitted effects. A failed DOM application
starts none of that frame's effects.

### Fail-closed lifecycle

Protocol validation and DOM application have three states: `live`,
`disposing`, and `disposed`. Any malformed application output:

1. applies no operation from that frame;
2. starts no effect from that frame;
3. removes delegated listeners and cancels live host work;
4. calls `__glamour_dispose` once when it is safe to enter Wasm;
5. clears host-custodied secrets and template-instance metadata;
6. reports a bounded diagnostic without echoing secret or untrusted payload
   contents.

The host never retries malformed binary output through the JSON interpreter.
That would turn memory corruption or a compiler/runtime bug into a second,
different execution path.

### Development and differential oracle

Development builds may include an explicit model snapshot export and trace
offset. Production builds omit both by default and the manifest records their
absence.

For differential tests, one message trace runs against:

- interpreter plus JSON reference host;
- compiled Wasm plus JSON reference host;
- compiled Wasm plus this optimized host.

After every step, tests compare normalized DOM, commands, subscriptions,
security decisions, and lifecycle state. They do not require binary operation
sequences to match JSON diff implementation details.

The binary decoder is fuzzed independently. Generated valid frames are also
mutated at every integer, offset, length, tag, count, identity, UTF-8, and
sequence boundary. A rejected frame must not mutate a fake or real DOM.

### Limits

The build manifest declares limits no larger than host hard maxima:

- input and output bytes;
- operations per frame;
- UTF-8 field bytes;
- template instances;
- live nodes and regions;
- keyed entries per region;
- pending effects and subscriptions;
- bytes and nesting per private callback environment;
- aggregate bytes across pending effect and subscription environments;
- dispatches per browser turn.

The compiler rejects a manifest request above a host maximum. The host may use
lower deployment limits. Guest admission checks the pending-entry counts and
private-environment limits transactionally before emitting work, so rejected
work is never visible to the host. Limit failures are deterministic and fail
before DOM application or host work.

### Performance acceptance

Phase 3 is not complete merely because the binary path works. Controlled
benchmarks must show:

- a one-text-slot counter update emits one structural operation;
- dispatch bytes are independent of unrelated model size;
- listener count is proportional to event classes and roots, not nodes;
- a stable workload reaches a bounded Wasm memory high-water mark;
- keyed reorder moves no more retained nodes than the
  longest-increasing-subsequence bound;
- optimized and JSON reference DOM traces agree;
- no accepted threshold regresses without an owner and expiry date.

Exact machine classes and numeric release thresholds live in Glamour's checked
performance document so measurement changes do not rewrite this ABI.

## Alternatives

### Retain the model in JavaScript

This keeps the current compiler simple but repeats model serialization and lets
host code observe application state. It cannot meet RFC-0107 Phase 3.

### Export Witchy's internal pointers

Passing an opaque model pointer back through JavaScript could avoid
serialization, but it would expose lifetime and representation details and make
forged, stale, or cross-instance handles part of the public boundary. The
compiler-owned state slot is narrower.

### Import direct DOM calls into Wasm

Direct calls avoid a patch buffer but create a chatty authority-bearing import
surface and cannot validate a whole update before partial DOM mutation. Batched
checked output is easier to audit, fuzz, replay, and compare with static output.

### Use JSON while retaining Wasm state

This removes model round trips but still pays complete VNode serialization and
host-side diffing. It is a useful migration checkpoint, not the Phase 3
production protocol.

### Make the ABI a new language construct

An `actor`, `component`, or `island` keyword is unnecessary. The compiler can
validate and wrap ordinary typed functions generated from ordinary Glamour
values. New syntax would make the language harder to learn without improving
the runtime boundary.

## Drawbacks

The compiler gains a stateful export mode beside pure string exports. The
browser host becomes a security-critical binary decoder and DOM interpreter.
Template manifests become part of build identity and caching. Debugging binary
frames requires dedicated tools. One Wasm instance per mounted application or
island costs more initialization than sharing one unconstrained module
instance, though engines may share compiled code.

Atomic frame validation can require bounded temporary host memory. Preserving
selection, composition, and browser-owned control state makes structural list
operations more complex than replacing subtrees. The JSON oracle must remain
maintained until differential evidence is strong enough to retire its
production compatibility role.

## Prior art

The architecture follows the compiler-directed template and resumability
research already cited by RFC-0107, especially Elm's explicit application
state, Svelte and Solid's compiled/fine-grained DOM work, Lit's stable parts,
Leptos and Dioxus's typed Wasm applications, and Qwik's serialized identity
model. The narrow length-delimited ABI and full-buffer validation also follow
ordinary defensive binary-protocol practice rather than exposing framework
objects across the trust boundary.

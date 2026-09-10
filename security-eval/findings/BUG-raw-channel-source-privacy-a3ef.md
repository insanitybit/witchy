# Raw channel bridges exposed message recovery without typed endpoints

Status: confirmed production-link privacy exposure, fixed in this change.
Baseline: `a3efae8df`. Evidence date: 2026-09-10.

## Exact boundary

External source could call or take first-class references to these ordinary
public std functions despite their compiler/stdlib-only message authority:

- `task.__channel_send_i64`
- `task.__channel_recv_i64`
- `task.__channel_select2_map`
- `chan.select2_frame_payload` and `chan.select2_frame_decode`
- `chan.select3_frame_payload` and `chan.select3_frame_decode`
- `chan.select4_frame_payload` and `chan.select4_frame_decode`

The last four decoders are included because they expose the same unbound
`Option(__Msg)` recovery as the select2 pair, not as a broader API inventory.

Source evidence on the baseline:

- `std/task.witchy:231` and `:235`: raw send/receive accept `ChannelId`, not a
  payload-parameterized endpoint. `:246`: raw select2 passes `Option(__Msg)`
  to its caller-provided continuation.
- `std/chan.witchy:131` and `:140`: the ordinary typed send/receive surface
  connects message erasure/recovery to the endpoint's payload parameter.
- `std/chan.witchy:515`, `:531`, and `:544`: frame decoders accept a tag and
  `Option(__Msg)` but no endpoint binding their result payload parameter.
  The payload-only equivalents are at `:561`, `:568`, and `:574`.
- `crates/witchy-syntax/src/intrinsics.rs::private_intrinsic_callers` protected
  the underlying erase/unerase intrinsics but had no privacy entries for these
  public source wrappers. Production linking consequently admitted them.

## Reproducer

This is the source fixture in
`linker::tests::raw_channel_privacy_wrong_payload_route_rejected`:

```witchy
import chan
import task
from chan import Receiver

fn wrong(rx: Receiver(Int)) -> task.Task(Option(String)):
    match rx:
        Receiver(id) -> task.__channel_select2_map(id, id, fn(_tag, raw): task.done(chan.select2_frame_payload(raw)))

async fn main(console: Console):
    let (tx, rx) = chan.channel(1).await
    chan.send(tx, 7).await
    let recovered = wrong(rx).await
    console.print("${recovered}")
```

On the baseline this passed production linking. The external continuation can
request String recovery from the raw message path associated with an Int
receiver, without preserving the typed endpoint relationship. The fixed linker
rejects the external `__channel_select2_map` call at the existing privacy gate.
Separate tests require the same privacy rejection for all six decoders, even
when supplied `None`, and for first-class references to all nine functions.

This confirms missing source-link authorization, not an executed runtime type
confusion, capability forgery, ownership violation, or full typechecker result.
Those stronger claims were not tested in this bounded syntax-only slice.

## Fix and preserved authority

`PRIVATE_MESSAGE_SOURCE_BRIDGES` lists only these nine qualified std targets.
The linker checks their resolved declarations, using the existing
`MESSAGE_BRIDGE_CALLERS` allowlist for authenticated `chan` and `task` modules.
User-provided std shadows are not mistaken for bundled std declarations.
These remain ordinary source functions: no new intrinsic IDs, type recipes,
lowering recipes, or casts. Direct calls and first-class function values,
including bare imports, cross the same declaration privacy check.

Positive production-link tests retain access for bundled authenticated std
modules, ordinary typed channel send/receive source, and compiler-owned
definition-site references to every frame decoder. The definition-site fixture
uses `mark_definition_site_expr`; it does not manufacture a source-spellable
authority marker. No std source, scheduler, WIR, canonical lowering, typechecking
pipeline, Unit storage, or runtime closure storage was changed.

Privacy is not a canonical message type witness. Explicit checked-tag recovery
is still required when lowering erased messages, independently of this fix.

## Integration follow-up: declaration identity and bare imports

The initial bare-name restriction also rejected an unrelated user function
named `select2_frame_payload`. A focused positive case reproduced that false
rejection. Moving the restriction to resolved std declarations revealed another
missing path: `from chan import select2_frame_payload` followed by binding the
function as a value was not resolved or checked like a direct call. The focused
negative case reproduced that source-link admission; it does not establish an
executed runtime exploit.

Function-value resolution now uses the same checked target resolution as calls.
The added cases require both direct and first-class user functions to remain
usable, while direct and first-class bare imports of the std decoder refuse.

## Focused evidence

Worktree: `.claude/worktrees/raw-channel-privacy-a3ef`.
Branch: `security/raw-channel-privacy-a3ef`.
The absolute target was CoW-seeded before Cargo:

```sh
cp -cR /private/tmp/witchy-rfc0165-reflect-specialization-target /private/tmp/witchy-raw-channel-privacy-a3ef-target
```

The only syntax test selector used, for baseline and fixed runs, was:

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= CARGO_TARGET_DIR=/private/tmp/witchy-raw-channel-privacy-a3ef-target taskpolicy -c utility cargo test -p witchy-syntax --lib raw_channel_privacy_ -- --nocapture
```

- Baseline production code, after correcting the positive fixture: 2 passed,
  4 failed, 422 filtered out. All nine raw functions were admitted; the
  wrong-payload fixture also passed production linking unexpectedly.
- With the privacy entries: 6 passed, 0 failed, 422 filtered out; exit 0.
  Four negative tests reject at the exact intended privacy diagnostic, and
  two positive tests retain authenticated std/generated access.
- An initial wrapper-enabled attempt failed before compilation because
  sandboxed `sccache` returned `Operation not permitted`; the command above
  disables that wrapper. No broad gates, formatter, or runtime tests were run.

# First-class affine Console authority

`Console<Write>` is an opaque affine value. It can be transferred through private
parameters, results and supported Result alternatives. It is not Copy. The
trusted public entry supplies handle 1 only when `main` explicitly declares its
Console parameter; `run` still takes no arguments and returns no values. A
private function cannot call main or manufacture entry authority.

```text
fn relay(c: Console<Write>) -> Console<Write>:
    return move c

fn log(c: &Console<Write>, n: U64):
    c.print_int(n)

fn main(console: Console<Write>):
    var output = relay(move console)
    log(&output, 1)
    output = relay(move output)
    output.print_int(2)
    discard(move output)
```

Printing uses any properly typed, available owned Console name or shared Console
parameter. `(*reference).print_int(n)` is also explicitly checked. The old magic
main-variable receiver path is removed. Main's Console is a typed local initialized
by a private entry IR operation, and printing reads that typed receiver. Selected
Console type use conservatively requires the output import, including private
helpers. The host's actual `--output` grant remains a separate requirement; no
source value can authorize itself or add external rights.

## Moves, availability and scopes

`move name` transfers a noncopyable binding. A plain name read cannot copy an
affine Console or an affine Result. Calls and constructors may produce fresh
owned rvalues directly. Moving a Copy binding keeps ordinary copy semantics.
`discard(move value)` consumes discardable authority; the builtin is reserved
against selected declarations and respects local shadows. It does not revoke a
host grant, close a heap resource or promise a destructor callback.

Every binding has a logical slot ID independent of its register count or lexical
name. Availability is tracked separately from saved name scopes as Available,
Consumed or Maybe. A missing logical owner fails closed. Restoring lexical scope
never restores an outer owner's availability. Possible live paths remain in
Maybe state; moves, borrows and uses after possible consumption refuse.

If and match alternatives start from isolated copies of the incoming flow and
merge only continuing paths. A returned branch cannot poison a still-live owner
on the other path. Bindings created by a scope expire/remove their own logical
slots while outer move effects survive. All selected/unreachable source remains
checked. Mutable owner replacement checks/evaluates its RHS before marking the
slot available again, so `slot = relay(move slot)` works; a rejected RHS never
resurrects availability. Replacement is limited to mutable local owner slots,
not references/projections.

Loop backedges conservatively require exact restoration of outer availability
from before condition evaluation. This includes condition effects: a continuing
body must restore anything consumed by its condition before the next iteration.
A body that always returns has no backedge. Normal loop exits retain the flow
AFTER condition evaluation, including the false/zero-body exit. Short-circuit
operators merge skipped and evaluated RHS flows; they do not pretend an optional
move happened on every path or silently undo one. Constant conditions do not
refine ownership alternatives in this slice, so some safe programs can refuse.

Only explicitly discardable supported values may expire implicitly. The central
properties source marks Console affine/discardable and computes Result traits
from its actual arm types. It never treats arbitrary non-Copy values as droppable.
Current normal scope/function exits discard remaining affine authority logically;
IR records explicit moves, replacement and expiry. Expiry needs no machine load
or callback, including a Maybe state, so it does not materialize a dead owner.
Future nondiscardable/linear types remain unsupported until their stronger
cleanup and flow rules exist.

## Shared capability access is not an owner copy

`&Console<Write>` parameters, temporary arguments and forwarding are supported;
mutable Console references are refused. Printing implicitly shares its receiver
through evaluation of ALL arguments. Thus
`console.print_int(close(move console))` refuses. An earlier `&console` argument
likewise blocks a later/nested move of the same owner. Borrowing moved or maybe
moved authority refuses.

A borrowed Console cannot be dereferenced, moved or copied into an owned source
value: `return *p`, `move p` and `move *p` refuse. A private physical frame load
used by the checked print intrinsic is only carrier access under shared
permission. References still cannot be stored or returned.

Console's nominal physical carrier is i32. An addressed standalone owner uses
one four-byte, four-aligned frame cell under existing full-frame guards. Shared
access adds only i32.load/store instructions (offset 0, natural alignment) to the
frame profile. Typed storage does not grant source Copy permission. Console is
still forbidden in records, arrays and concrete record arguments, including
phantom arguments. Unsupported rights, constructors, integer conversions,
field/tag projections, owner records and arbitrary resource storage refuse.

## Affine Results

A Result is Copy only when both alternatives are Copy. A Console alternative
makes the whole Result affine; both supported alternatives must be explicitly
discardable for implicit expiry. Constructors consume only their active owner.
Inactive machine zeroes are physical ABI padding, never owners or cleanup inputs.
Noncopyable Result names require `move` when passed, returned or matched:

```text
fn unwrap(r: Result<Console<Write>, Console<Write>>) -> Console<Write>:
    match move r:
        Ok(output):
            return move output
        Err(output):
            return move output
```

Match consumes the result and introduces an available owner only inside the
active Console arm. Invalid tags still trap; sibling scopes and control-flow
states remain isolated. Unit alternatives and existing copyable Results keep
their semantics. Sum nesting/storage and generic functions remain unsupported.
Discarding an affine Result expires its logical active authority only; there is
no inactive-owner construction or heap cleanup callback.

## Binding and limitations

Optional `capability_types` records Console<Write>, ownership affine, i32 carrier,
frame size/alignment 4, `record_storage: false` and entry `main-output-grant`.
It is omitted without Console type use. Sum metadata adds ownership
`copyable`/`affine` and explicit `implicit_discard`. Existing schemas, exact source,
compiler/host/module/receipt binding and actual output grants remain in force.
No new memory map, growth, export, raw entry, grant or proof certificate is added.

Compiler source controls cover ownership flow, scopes, joins, receiver/argument
loans, loop/condition effects, affine Results and storage/forgery refusals. Actual
host execution, inactive alternatives, copied-frame isolation, malformed module
surfaces and artifact/publication controls need the separately coordinated focused
checks. Authored scalar lemmas retain fresh checking after runtime/type/flow
checks; a valid lemma cannot excuse a bad move. This is not heap ownership,
Allocator/Buffer/Vec/String, destructor cleanup, unsafe proof or reusable generic
admission completion. Traps still discard the instance without guest unwinding.

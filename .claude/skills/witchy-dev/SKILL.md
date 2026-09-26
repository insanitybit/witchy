---
name: witchy-dev
description: The baseline briefing for developing in this repo — enough witchy language, architecture, code-organization, and merge-queue protocol to do ordinary feature work correctly without first learning the whole language. Use at the start of any coding task here (Rust compiler crates or `.witchy` code) when you have not already loaded this context.
---

# witchy-dev — the baseline briefing

Everything you need for ordinary feature work is on this page. Don't go
exploring to reconstruct it. Reach for deeper sources only when this page is
genuinely insufficient — pointers are at the bottom.

**Assume you are one of several agents working this repo right now.** That is
the normal case, not an edge case. Other agents hold branches, submit to the
merge queue, and have uncommitted edits in the shared checkout *while you work*.
So: nothing you observed is guaranteed still true when you act on it, changes
you didn't make are someone's work-in-progress rather than mess to clean up, and
master moves under you mid-task. §3 has the mechanics; the rule of thumb is
**re-check before you act, and never undo what you didn't do.**

## 0. The one rule: converge before support

Stable observable semantics converge before support. RFC-0167 supersedes the
dual source-runtime rule in RFC-0135: checked canonical SafeWIR is the single
language implementation and admitted Wasm is the production runtime.
Independent expected results adjudicate correctness.

- **Stable language semantics:** an independent expected result must agree with
  admitted unoptimized Wasm, with focused structural checks at the affected
  compiler and runtime boundaries.
- **Source tree walker:** frozen for language semantics. Preserve its temporary
  comptime/build duties, but do not add RFC-0167 views or new runtime semantics.
- **Pure optimizations** (SIMD, SROA, bounds elision): compare optimized Wasm to
  admitted unoptimized Wasm and the expected result.
- **Capability, confinement, ABI security:** strict differential
  coverage. No debt.
- **Full canonical conformance corpus:** stabilization, release, and periodic
  CI, not every experimental slice.

Disagreement is a **loud compile or runtime error, never a different answer.**
If you add user-visible stable behavior, add an independent expected result,
exercise admitted unoptimized Wasm, and pin the affected structural contracts.
Do not use the source interpreter as RFC-0167 acceptance evidence.

## 1. witchy the language — the 90% rundown

**It is a novel language in no training data.** Do not pattern-match it to
Python or Rust from memory. What follows is accurate; recalled intuitions are not.

**Layout is significant** (off-side rule): blocks open with a trailing `:`, 4
spaces per level. Everything is an expression; a block's value is its final
expression. Comments are `//` and `/* */`.

```witchy
fn classify(n: Int) -> String:
    match n:
        0 -> "zero"
        _ -> if n > 0: "positive" else: "negative"

fn main(console: Console):
    console.print(classify(-3))
```

**Capabilities are the heart of it.** Authority enters a program in exactly one
place: the typed parameters of `main`. Capabilities cannot be constructed (no
constructor exists), flow only through values (no globals, no ambient lookup),
and only ever **narrow**. Capability parameters expose directly possessed root
authority. Ordinary callbacks are values too: passing one delegates its opaque,
potentially effectful callable behavior without handing the receiver arbitrary
access to the capabilities captured by its creator. Therefore no capability
parameters means no direct host authority, not "provably no effects." Audit root
demand and callable interfaces from signatures; do not infer an ordinary
`fn(...)` value's hidden capture set. The checked `pure fn` qualifier is the
explicit effect-free contract. Capability operations are **methods on the
capability that carries the authority** - `console.print(s)`, `dir.read(path)`,
`clock.now()` - so direct authority is loud at every call. The caps: `Console`,
`Clock`, `Env`, `Rand`, `Dir`, `File`, `Net`, `Fetch`, `Exec`, `SecretStore`,
`Secret`. Narrow with `dir as Dir[Read]`,
`dir.subtree("uploads")`, `net.only(Net.tcp(host, port))`,
`net.deny(Net.private())`. To deny direct possession to a region of code, give
it its own function and do not pass or capture the capability. Also avoid
delegating a callback that exercises that authority: the firewall is the full
flow of values, not merely the absence of a capability-typed parameter.

**Types.** `Int` (64-bit, wraps on overflow), `Float`, `Bool`, `String`,
`Duration` (`30s`, `250ms` — a distinct type, not an `Int`), `Nil` (both the
unit type and the unit *value* — you write `Ok(Nil)`, not `Ok(nil)`), `List(a)`,
`Dict(k, v)`, tuples. One `type` declaration covers enums, tagged unions, and
records:

```witchy
type Shape:
    Circle(Int)
    Square(Int)

type Account:
    name: String
    balance: Int
```

Records construct positionally or by identifier name (`Account(name: "bob", balance: 5)`, `.{ host: "127.0.0.1", port: 8080 }`),
read by `.field`, and update by spread (`Account(balance: n, ..acc)`). Record/struct
fields are static identifier keys (not string literals).
`type X = …` names a shape and never mints a type; `type X: …` mints a nominal
type. `sealed type` confines *construction* to the defining module (field reads
and `match` still work anywhere) — that's how invariants get one choke point.

**Boolean operators** are C/Rust style: `&&`, `||`, `!`. Python keywords `and`,
`or`, `not` are rejected with targeted compiler diagnostics. Length across all
strings and collections is universally `.length()` (`s.length()`, `xs.length()`).

**Value semantics, everywhere.** Every boundary that carries a value out of a
scope — a call argument, a closure capture, a task message — carries a **copy**.
There is no shared mutable state. `var` is a *local* mutable binding; the one
mechanism that writes back to a caller is a `var` **parameter** (move-in /
move-out). Closures capture by value and **cannot assign to a captured
variable**. Parameter conventions: default (owned, immutable), `let`
(non-escaping borrow), `var` (write-back), `own` (callee consumes), and
`move e` at the use site (caller consumes).

A non-`var`, non-`Nil` call result that you throw away is a **compile error** —
bind it or `let _ = …` it.

**Failure is a value; there are no exceptions.** `Result(T, e)` / `Option(T)`
with `?` to propagate, `? "msg"` to propagate with String context, and `??` as a
lazy unwrap-or-default. Unexpected failure — out-of-bounds, `/0`,
`string.to_int` on junk, NaN ordering, `fail(msg)` — **aborts in every canonical
execution** (a runtime error in the SafeWIR interpreter, a trap in admitted
Wasm). There is no truthiness: `""` and
`[]` are values, not absences.

**Pattern matching** has one grammar used in every binding position. `match` is
exhaustiveness-checked and rejects unreachable arms. Patterns: `_`, a variable,
literals, integer ranges (`0..10`, `0..=10`), tuples, constructors, list shapes
(`[]`, `[first, ..rest]`), and or-patterns (`1 | 2 | 3`) nestable anywhere.
`let`/`for`/comprehensions require an **irrefutable** pattern — a refutable one
there is an error telling you to use `if let`. `Float` literals cannot be
matched; `Duration` literals can.

**Generics and traits** are Rust-flavored: `fn largest(xs: List(a)) -> a where
a: Ord`, `trait`/`impl … for`, `x: impl Loud` as argument-position sugar,
`derive(Show, PartialEq, Eq, PartialOrd, Ord, Reflect, Deserialize,
PublicState)`, and `dyn Trait` existentials. Generic functions are checked once
and monomorphized per concrete use.

**Modules.** `list`, `string`, `dict`, `math`, `option`, `result`, `policy`, and
`show` are **the prelude** — no import line. Otherwise `import name`. A standard
type's operations are **methods** (`xs.map(f)`, `s.to_upper()`,
`d.get(k)`) — that's the primary form; every public method also has a
module-qualified alias (`list.map(xs, f)`). A module's `pub` types are qualified
(`json.JsonInt(1)`) unless you `from json import Json`.

**Concurrency** is Go/CSP: `async`/`await`, `chan.spawn`, first-class typed
channels, on a cooperative executor written in witchy. Tasks share no memory, so
no locks and no data races, and the round-robin schedule is **deterministic** in
admitted Wasm. Concurrency
stays *inside* one VM; isolating
untrusted code means running it as its own sandboxed program, not as a task.

**Writing witchy — the traps that actually bite:**

- A receiver bound by `?`, `if`/`match`, or a generic return often needs a
  `: Type` annotation before a trait method will dispatch on it
  (`let top: Entry = heaviest(xs) ? "picking top"`). The type error tells you.
- ` ```witchy ` blocks in `book/`, `README`, and `spec/` are **executed tests** —
  each must be a complete, correct program. Use an untagged or ` ```sh ` fence
  for partial snippets. ` ```witchy-static ` is **not** executed, so those blocks
  rot silently — hand-check them. And don't document an API that doesn't exist;
  the build catches wrong code but not wrong prose. Writing or auditing user
  docs is **`/witchy-docs`**.
- `spec/stdlib.md` is **generated** from `std/*.witchy` doc-comments and a test
  fails if it drifts. Edit the doc-comment, then
  `witchy doc std/*.witchy > spec/stdlib.md`. Never hand-edit it.

**Verify, don't assume:** `witchy <file>` (admitted Wasm run), `witchy check <file>`
(type-check), canonical conformance tests (SafeWIR interpreter plus admitted
Wasm), `witchy caps <file>`
(capability footprint), `witchy test <rune>`, `witchy fmt <file>`,
`witchy which <name>` (find a stdlib symbol).

## 2. Architecture and code organization

**The compiler is a Cargo workspace**: eleven stage-aligned crates under
`crates/` plus the `witchy` root package. Build, lint, and test the **whole
thing** — `cargo nextest run --workspace`, `cargo clippy --workspace
--all-targets`. The root lib re-exports every crate's modules, so
`crate::{ast,typeck,codegen,…}` paths still resolve from the binary — **but new
code belongs in the owning crate.**

| Crate | Owns |
|---|---|
| `witchy-cap-model` | canonical capability names/classes/rights vocabulary (dependency bottom) |
| `witchy-testkit` / `witchy-test-host` | backend-neutral fixture plans; the shared fixture host |
| `witchy-syntax` | lexer, parser, AST, formatter, linker, AST-level passes |
| `witchy-types` | typeck (HM unification), traits + monomorphization, runtime types |
| `witchy-wir` | canonical SafeWIR, layouts, structured IR, peephole, wasm encoder |
| `witchy-lower` | `codegen` (AST → WIR) and `analysis` (uniqueness / cap tokens) |
| `witchy-confinement` | target-neutral confinement policy + platform providers |
| `witchy-runtime` | runtime `Value`, native registry, Wasmtime sandbox |
| `witchy-interp` | frozen source tree walker; temporary `comptime`/build and tagged-literal compatibility services |
| `witchy-caps` | footprint analyzer, grant documents |
| `witchy` (root) | CLI, LSP, wasm playground — composition only |

`std/` is the standard library, written in witchy. `projects/grimoire` and
`projects/coven` are the package manager and registry, self-hosted in witchy.

**Ports and adapters is the house style.** Define the domain contract in a
target-neutral crate and let platform-specific adapters consume it — never the
other way around. Two exemplars already in the tree: `witchy-confinement`
defines normalized policy and the Linux Landlock/seccomp provider consumes it
without depending on compiler stages or Wasmtime; `witchy-testkit` defines
backend-neutral plans and Wasmtime/browser adapters consume
`witchy-test-host` rather than reimplementing fixture semantics. Follow that
shape: policy and contracts point *down* the dependency DAG, enforcement points
*up*.

**Keep files small and single-purpose.** Some existing files are very much not
(`codegen/mod.rs` is ~10.5k lines, `typeck.rs` ~9.6k) — those are known debt
tracked in `spec/architecture-ledger.md`, not a pattern to imitate. Add a new
module rather than growing a giant one, and put the seam where the domain has a
seam.

**Two standing design rules:**

- **Never special-case a method.** Memory / in-place / reclamation
  optimizations are driven by the ownership conventions (`let`/`var`/`own`,
  `frozen`/`unique`) plus the escape/uniqueness analysis — **one general
  mechanism for all operations.** Add **no new** `*_cap` runtime helpers and no
  new per-method `self_*` recognizers in `crates/witchy-lower/src/analysis.rs`.
  A per-operation hack doesn't generalize, and the ops that don't get it
  silently regress. The existing zoo is **retained, not deleted** — RFC-0051
  measured removing it and it was perf-negative — but it does not grow.
- **Trait dispatch is typed and table-first.** `show(x)` resolves by reading the
  receiver's concrete type from typeck's `TypeTable` — the real inference
  judgment, not a string guess. A dispatch fix belongs in the **typed path**
  (make the checker type the expression so the table carries it), never in a new
  string-shape table.

**Rust style:** the Rust here is **hand-formatted on purpose. Never run
`cargo fmt`** — it reformats ~70 files. The only formatting gate is
`witchy fmt` over `std/*.witchy`, `examples/*/src/*.witchy`, and
`projects/**/src/*.witchy`. Match the comment density, naming, and idiom of the
code around you.

**Where write-ups go:** bug reports → `bugs/` (gitignored local backlog, one
`BUG-NNN-slug.md`: symptom, repro, root cause, fix). Design decisions and
proposals → `rfcs/` (tracked, `NNNN-slug.md`). Security findings →
`security-eval/` (`SEC-NNN`, gitignored). Don't file a bug as an RFC or an
ad-hoc design doc as a bug.

## 3. Building and testing

**The `witchy` on your PATH is the RELEASE binary; `cargo build` produces
DEBUG.** `~/.cargo/bin/witchy` symlinks to `target/release/witchy`. So editing
compiler source, running `cargo build`, then testing with `witchy foo.witchy`
runs your **old** code and looks like your change did nothing. Either run
`./target/debug/witchy …` or `cargo build --release`.

**`cargo check -p witchy-runtime` does NOT compile the Wasm kernel.** The
`runtime` module sits behind that crate's non-default `native` feature, which
only the root package activates — a package-scoped check reports green without
ever parsing those files. Validate runtime work with `cargo check -p witchy` or
a workspace-wide command.

**This checkout is shared by several agents at once — plan for it.** Treat the
worktree, `target/`, master, and the merge queue as shared state:

- Run `git status --short --branch` before editing and again before claiming the
  repo is clean. If files changed under you, another agent or the user did it —
  work with those changes.
- Say which files you're editing. If another agent touches the same hunk, stop
  and ask rather than overwrite.
- **Never** revert, delete, or reformat changes you didn't make. No
  `git checkout --`, `git reset --hard`, `git stash`, `cargo fmt`, or
  `rm -rf target` as a cleanup shortcut — and that ban is not only about
  wiping a dirty tree. It includes **do not retarget `master`.** Clean up
  only artifacts you created.
- Don't kill Cargo, nextest, dev-server, or browser processes you didn't start.
- Use a per-agent target dir for long commands:
  `CARGO_TARGET_DIR=target-claude cargo nextest run --workspace`. Run ad hoc
  cargo through the repo shim (`./scripts/bin/cargo`, or `scripts/bin` first
  on `PATH`): it routes builds through the `mbx` build cache so a fresh
  target dir restores dependencies instead of recompiling them.
  `agent-check.sh` does this for you. `WITCHY_BUILD_CACHE=off` turns it off.

**Keep the blocking suite fast.** Treat roughly two seconds per ordinary test
as a useful optimization target: first measure the slowest tests, then reduce
fixture size, remove redundant setup, or move genuinely long compiler/Wasm,
integration, and fuzz campaigns to an explicitly named nightly/non-blocking
job. This is guidance, not a correctness gate; do not weaken assertions or
hide tests with `#[ignore]`. Preserve a discoverable runner path and record
why a test remains long.

**Uncommitted changes you didn't write are the common case, and they are
someone's in-flight work.** Don't discard them and don't silently fold them into
your own commit. Read the diff and decide: unrelated to your task → leave it
alone and say so in your report; a fact you can check (a count, a filename, a
path) → verify it, and if master has moved since it was written, correct it and
commit it on its own branch with a message that says what you verified.

**Anything you measured has a shelf life.** Test counts, census numbers,
artifact filenames, queue contents, and "the queue is empty" all go stale within
minutes. Re-read state immediately before you act on it, and re-derive a number
before you write it into a doc rather than copying a number someone else wrote.

**Others' work in the queue is not yours to fix.** A branch that goes RED, a
duplicate submission, a diverged remote — report it with the log path and leave
it. The exception is a genuine duplicate of the *identical* SHA, which wastes a
whole gate cycle: `drop` it with a journaled reason.

**Your own coordinator outlives your task.** If you started
`merge-queue.sh run`, it keeps gating whatever else is queued after your branch
lands. That's fine and usually desirable — just say so in your report so the
next agent knows a coordinator is alive rather than starting a second one.

**All work happens in a worktree on a branch — never edit the main checkout
directly**, which blocks other agents. The shared checkout is the directory
whose `HEAD` is `master` (the path `git worktree list` shows first). Check
out your branch with `git worktree add -b <branch> .claude/worktrees/<name>`
(or `.codex/worktrees/<name>`), not by switching that directory off `master`.

**`refs/heads/master` is the merge-queue landing ref. You do not move it.**
Only the coordinator fast-forwards it. In particular:

- **`origin/master` is not this repo's master.** GitHub is thousands of
  commits behind the local `master` the queue lands on. `git fetch && git
  reset --hard origin/master` (or any reset of `master` onto the remote) is
  a production outage, not a sync.
- Do not **rename**, **reset**, **checkout -B**, or **branch -M** `master`
  to turn the shared checkout into a feature branch. That is how you get a
  worktree: `git worktree add -b <branch> <path>`.
- Do not check an agent branch out in the shared master directory. Two
  checkouts of `master` are also forbidden by git; renaming `master` to
  dodge that is the same crime.

Fresh worktrees need cache preparation: agent worktrees get it automatically
via the `WorktreeCreate` hook. Cargo 1.100 and mbx leave a managed `target`
symlink and copy registry/Git dependency units from another checkout; a
recycled pool path retains its learned incremental state. For a manual worktree
run `./scripts/worktree-warm.sh <path>` to seed the non-Cargo verdict cache and
leave the default target to mbx. Explicit `CARGO_TARGET_DIR`s still need
`./scripts/worktree-warm.sh --target-dir <dir>`. Never point one at a shared
directory — Cargo's build lock would serialize concurrent agents.

**Do not build a fresh `CARGO_TARGET_DIR` under `/private/tmp` per task.** A
cold target is 30-60 GB, and abandoned ones filled the disk twice in one
week (117 MB free). If a worktree's own `target` is not enough, CoW-clone a
warm one (`cp -c -R .claude/worktrees/merge-gate/target-prewarm
/private/tmp/witchy-<task>-target`; it shares blocks until you build), keep the
`witchy-<task>-target` name so it is recognisable, and delete it when your
branch lands. A debug `witchy` built into a target outside the checkout
reads std from disk and cannot find it, so run it by hand with
`WITCHY_STD_DIR=<checkout>/std` (tests get it via `CARGO_MANIFEST_DIR`). `scripts/reclaim-build-caches.sh` (dry run by default) reclaims
what gets left behind: worktree caches by branch state, and standalone
`/private/tmp/witchy-*` caches once they are 48 hours idle with no open
handle. Judge by `df`, never `du` — a clone reports its full size.

## 4. Landing work: the merge queue

`./scripts/check.sh` is the local green gate (build + clippy + `nextest
--workspace`). But **never run two full gates at once**, and
**never merge to master while a full gate is running** — it invalidates that
gate. The coordinator (`scripts/merge-queue.sh`) enforces both. Protocol:

1. **In your worktree, run focused checks**, not the full gate. After committing
   the exact tip you intend to submit, run `./scripts/agent-check.sh edit`; it
   selects the edit-loop checks from the complete committed diff and records the
   commit-keyed presubmit receipt. `./scripts/test-for-paths.sh --run` remains
   available for unusual explicit path sets (pass `--focused` to restrict
   strictly to touched crates/tests and avoid full workspace builds during
   inner-loop iteration). Shard building blocks:
   `./scripts/check.sh --fast` (tests, clippy overlapped in the
   background), or `--examples` for the example matrix. A green shard
   **qualifies** you for the queue; it doesn't replace the full gate.
2. **Submit:** `./scripts/merge-queue.sh submit <branch>` — or
   `submit --after <parent-branch> <branch>` if it's stacked on another queued
   change.
3. One coordinator session runs `./scripts/merge-queue.sh run`. It rebases each
   candidate onto latest master in a dedicated warm worktree, runs the full gate
   under a lock, and fast-forwards master on green. If master moves mid-gate it
   re-rebases and re-gates rather than merging a stale validation.
4. Any **ad-hoc heavyweight suite** shares the same lock:
   `./scripts/merge-queue.sh with-lock -- <cmd>`.
5. `./scripts/merge-queue.sh status` prints machine-readable state. Queue,
   journal, gate logs, and lock live under gitignored `state/merge-queue/`.

The `/land` skill drives this end to end. `scripts/MERGE-QUEUE.md` is the
operator's guide — read it before debugging or extending the queue.

**Adversarial review is selective.** Before submitting a branch that touches
canonical-runtime contracts (typeck, codegen, SafeWIR interpreter, `analysis.rs`, the
wasm ABI), have a fresh-context reviewer read **only the diff** and try to
reject it: every hunk must trace to the stated task, and green tests are not
proof for changes the differential suite doesn't adjudicate. Skip the ceremony
for changes the existing suites already adjudicate — docs, std functions with
differential tests, ledger closes.

## 5. Working style

- **Follow the active user goal and the live merge queue.** Don't self-select
  bugs from `bugs/README.md` without asking — that ledger doesn't encode
  priority. `scratch/RELEASE-QUEUE.md` is a historical snapshot, not a current
  priority source.
- **Fix the generator, not the output.** A mistake made twice by an agent is a
  prompt/docs bug — fix it with a line in `CLAUDE.md`, this skill, the relevant
  RFC, or the queue entry, not by hand-patching only the latest instance.
- **Update generated evidence with the change that invalidates it.**
- **Batch changes, test sparingly** — group a handful of edits before running a
  suite.
- **Break, don't deprecate:** one-cut migrations, no compatibility layers.
- **Make the call.** Choose on trade-offs and state your assumption rather than
  handing the decision back.

## Going deeper

Only when this page is genuinely insufficient:

- **`/learn-witchy`** — the full guided read of the language, in priority order.
- `spec/language.md` — the authoritative syntax + semantics reference
  (executable examples). `spec/capabilities.md` — the security model.
  `spec/stdlib.md` — generated module-by-module API. `spec/architecture.md` —
  pipeline and workspace layout.
- `CONTRIBUTING.md` — build/test/conformance. `docs/agile-agent-playbook.md` — agent
  process, boundaries, handoff format. RFC-0167 is authoritative where it
  supersedes RFC-0135's former dual-runtime requirement.
- **Always check recent RFCs (`rfcs/`) for new language features.** The spec
  (`spec/language.md`) often lags behind newly implemented RFCs; RFC-0167 removes
  explicit lifetimes and mutable references in favor of immutable rooted views.
- `book/src/` — narrative tour, ordered by `book/src/SUMMARY.md`.
  **`/witchy-docs`** is the skill for writing and reviewing it (and `spec/`):
  what may never appear in user docs, the executed-vs-static block trap, and the
  house voice.
- Source of truth when docs disagree:
  `spec/` + `std/*.witchy` + independent expected results
  (`src/example_tests/`, `tests/misc/semantic_conformance.rs`). The canonical
  SafeWIR interpreter is the executable oracle; the source tree walker is not.

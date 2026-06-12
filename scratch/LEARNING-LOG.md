# Learning Log: A Fresh LLM Learns Witchy

Audience: language designers. I (an LLM, no prior witchy memory) read
`book/src/**` cover-to-cover, then wrote a graduated series of real programs in
`scratch/`, with reduced "probe" files for surprises. This log is the deliverable.

## Severity key

- **Blocker** — couldn't proceed using only the book.
- **Friction** — book was unclear or wrong; cost real time.
- **Papercut** — small surprise, quickly worked around.
- **Worked-well** — call-outs where the language/tooling delighted.

---

## Top findings (prioritized)

### 1. `derive(...)` and `comptime:` are completely undocumented in the book — Friction

Tasks #40 in the project memory mark `derive(Show,Eq,Ord) + comptime v1` as
shipped, and the docs/language.md reference covers both — but `book/src/**`
mentions neither (grep confirms zero hits). I had to fall back to
`docs/language.md` § 8 to learn both. Recommendation: add a "Convenience"
sub-chapter after Generics/Traits with `derive(...)`; add a "Compile-time code
generation" sub-chapter before or alongside the Tour-iterators chapter.

### 2. Comprehensions `[expr for x in xs if cond]` are documented only in `docs/language.md`, not in the book — Friction

`docs/language.md` § 10 introduces comprehensions. The book's `tour-iterators.md`
shows `iter.filter`/`iter.map`/`iter.collect` but never comprehensions. They are
arguably the most ergonomic everyday tool I reached for. Add to
`tour-iterators.md` or `tour-values.md`.

### 3. Inline `match` arms with `->` body cannot contain `return` or assignments — Friction

```witchy
// Compiles:
match x:
    0 ->
        return Err("zero")
    _ ->
        Ok(n * 2)

// Parse error: "expected an expression, found `return`"
match x:
    0 -> return Err("zero")
    _ -> Ok(n * 2)

// Also parse error: "expected a pattern, found `=`"
match Event.parse_line(line):
    Some(e) -> out = list.push(out, e)
    None -> ()
```

In other words, the `arm -> expr` form requires the body to be an *expression*,
not a statement. Once you drop the `->` and indent the arm body on its own line,
both `return` and assignment work fine. The error message ("expected an
expression, found `return`") points at the right place but doesn't explain the
distinction between expression-form and block-form arms. See
`probe_return.witchy`. Recommendations: (a) accept statements on the right-hand
side of `->`, or (b) flag this in the book's match section.

### 4. `Dir.write` SEMANTICS — replace-not-append, but book and stdlib comments suggest append — Friction

`book/src/capabilities-optional.md` says "Append a line if we were given somewhere
to write" but the call is `write(d, name, line)` — which actually *overwrites*
the file. My `11_auditor_actor.witchy` recorded three lines and the file ended
with only the last one. There appears to be no first-class `append` primitive on
`Dir`. Either:

- Document write as overwrite-only and add an `append`-style helper, or
- Fix the misleading comment in `capabilities-optional.md` line 15.

### 5. `let _ = expr` does not parse — Papercut

```witchy
let _ = recv_all(sock)   // parse error: expected an identifier, found `_`
```

Workaround is to name the binding (`let _bytes = ...`), but Rust users will
reach for `_` immediately. Either accept `_` as a discard pattern in `let`, or
document a `discard expr` form.

### 6. `\"` inside `${...}` interpolation fails (lex error) — Papercut

```witchy
print(console, "${equal(Score(1, \"x\"), Score(1, \"x\"))}")
// parse error: unterminated `${` interpolation
```

But a plain inner string works: `"${some_fn("ok")}"`. So the `\` is being eaten
by the *outer* string scanner. Either re-enter string mode inside `${ }` (most
ergonomic — the user expects escapes to work), or document the constraint. As a
new user I tried `\"` three times before I gave up and refactored to a `let`.

### 7. `fmt` desugars `if let` into `match`; book teaches `if let` — Papercut

`book/src/tour-errors.md` introduces `if let Some(v) = ... :` as an idiom. When
you `witchy fmt`, every `if let` is rewritten to `match v:` + wildcard.
Inconsistent: the book's "canonical" idiom is not the formatter's canonical form.
Either keep `if let` syntactically and don't fold it, or remove it from the book.

### 8. `fmt` rewrites `Inc()` (nullary variant ctor with parens) to `Inc` (bare) — Papercut

The actors chapter uses `send(counter, Inc())`. `fmt` rewrites this to
`send(counter, Inc)`. Both work; the book's form is not the canonical form. Tiny
inconsistency, but it means copy-pasting from the book and then running `fmt`
silently changes your code.

### 9. `witchy new` always scaffolds with a `main` (even for libraries) — Papercut

A scaffolded rune always has `main(console)`. For a library, you must delete
this and replace with `pub fn`s. There is no `--lib` flag. Minor, but a
discoverable one-liner would help: e.g. `witchy new mylib --lib` (or print a
"libraries: replace main with pub fns" line in `new`'s output).

### 10. The `why`/`tree`/`outdated` subcommands ignore `-C` — Friction

`witchy build/run/add/audit -C path` all work. But:

```
$ witchy why mylib -C /path/to/myapp
error: cannot read ./witchy.toml: No such file or directory (os error 2)
```

You must `cd` first. Inconsistent.

### 11. Build-step output is a *separate* generated module — undocumented mental-model trap — Friction

`book/src/packages-build.md` shows `write_out(out, "api.witchy", ...)` and says
the output "flows into the ordinary parse → link → type-check pipeline" — but it
doesn't say *where*. I assumed the generated functions would become part of the
host rune's module (so `genlib.generated_message()` from inside a `genlib.outer`
function). That is wrong. Instead, each generated file becomes its OWN module
that the consumer imports separately: my `build.witchy` wrote `generated.witchy`,
and the consumer needed `import generated` (NOT `genlib.generated_message`). I
found this by reading `tests/e2e.rs` after the book left me stuck.

This is a meaningful conceptual gap. Recommendation: a one-paragraph
"how generated code is reached" subsection in `packages-build.md` that says:

> A build step's output files (`name.witchy`) become *new modules* available
> to the rune AND its consumers via `import name`. They are not folded into
> the host rune's module.

### 12. The book's `dict` examples don't show an `import dict` — Papercut

`tour-values.md` shows `dict.new()`, `dict.insert(...)` without an `import dict`
line. In a real program, you need `import dict`. Same for `list` (the book is
inconsistent here — sometimes shows it, sometimes doesn't). Recommendation: be
consistent in the book examples.

### 13. The `caps` per-function breakdown is not as advertised — Papercut

The book (capabilities-authority.md) shows:

```
fetch   Net[Connect, Tcp]
load    Dir[Read]
serve   Net[Listen, Tcp]
main    Console, Dir, Net
total   Console, Dir, Net
```

In practice `witchy caps` only seems to print `main` and `total`:

```
Host-capability footprint of /Users/cobrien/workspace/witchy/scratch/14_event_summary.witchy:
  main   Console, Dir[Read]
  total  Console, Dir[Read]
```

Maybe a flag to expand? `--per-function`? At minimum the book overstates the
output detail.

---

## Worked-well callouts

- **`witchy parity`** — running on both backends and confirming identical
  output gave me confidence after every program; very nice tool to have. Worked
  out of the box on every program I wrote.
- **Error messages on capability narrowing** — clear, specific, and they name
  the right (`Write`, `Listen`) that was demanded. See `probe_narrow_fail.witchy`
  and `probe_net.witchy`.
- **The widening-gate UX** — `witchy update` and `witchy add` both gave precise,
  actionable messages when authority would widen, naming the right and even the
  flag to accept. The two-layer build-step default-deny (kind gate + grants
  section) is well-staged.
- **Sandbox structural enforcement** — running with `--dir scratch` then trying
  `../etc/passwd` failed with `\`..\` escapes the Dir capability` immediately.
  Trustworthy.
- **The `check` perf note** — `xs is rebuilt by copy on every iteration of this
  loop — it is bound to a new name` (probe_perf2). Surprisingly delightful;
  exactly the right level of feedback for the optimization model.
- **`derive(Show, Eq, Ord)` plus `say`** — `type Score derive(Show, Eq, Ord)` gave
  me a structural rendering and the `equal`/`less` functions for free. The
  combination of `say(console, x)` and `say(console, "${x}")` worked seamlessly.
- **Comprehensions** — once I found them in docs/language.md, `[e for e in events
  if e.severity == "ERROR"]` was the clearest line of `14_event_summary.witchy`.
- **`witchy test`** — simple, fast, capability-free. The `test_*` convention is
  zero-friction.
- **Actors** — the spawn + send + handler model with capabilities pinned at
  spawn was very natural. I expected friction; there was none. (`07_actors.witchy`,
  `11_auditor_actor.witchy` both compiled and ran first try.)

---

## Per-program notes

### `01_hello.witchy` — Hello world

Copied straight from the book. Worked first try, both interpreter and parity.

### `02_records_enums.witchy` — Records, sum types, match, list patterns, guards

Worked first try. The `..a` spread for records, `[first, ..rest]` list pattern,
and guard clauses (`m if m > 0`) all behaved as described. **Worked-well.**

### `03_generics_traits.witchy` — `where T: Ord`, `impl Show for T`, `impl Trait`

Worked first try. The `largest` generic compiled cleanly for both `Int` and
`String`. **Worked-well.**

### `04_errors_result.witchy` — `Option`, `Result`, `?`, `if let`

Worked first try, including `?` for both Result chaining and inside Option-typed
expressions. Later `fmt` rewrote my `if let Some(v) = first_even(...)` into a
`match`, which surprised me — Finding #7.

### `05_iter_generators.witchy` — `gen fn`, `yield`, lazy iteration

Worked first try. The infinite Fibonacci bounded by `iter.take` was beautiful.
The Collatz example illustrated unbounded loops cleanly. **Worked-well.**

### `06_capabilities.witchy` — `Clock`, `Env`, `Dir[Read]`, `retain`/`without`

Worked first try. `caps` reported `Clock, Console, Dir, Env`. Sandbox respected
all of them. Both `retain console:` and `without clock:` compiled and ran.

### `07_actors.witchy` — Counter, Worker, Boss, Subject

Worked first try. Parity passed.

### `08_methods_derive.witchy` — `impl Score: fn doubled(self)`, `Score.zero()`, `derive`

First attempt hit Finding #6: `\"` inside `${ ... }` failed to parse. Refactored
to use `let` bindings before the interpolation. Then worked. The `Score.zero()`
static method form is very pleasant.

### `09_comptime.witchy` — `comptime:` block with `emit`

Worked first try. I had to learn this from `docs/language.md` since the book
doesn't mention `comptime` (Finding #1). The combination of `comptime:` + string
interpolation inside `emit("    ${i * 7}")` is striking.

### `10_scan_logs.witchy` — The book's confined log scanner

Worked first try. `caps` reported exactly `Console, Dir[Read], Env` as
predicted. The path-escape sandbox check (`scan ../etc/passwd`) was rejected
with a clear error. **Worked-well.**

### `11_auditor_actor.witchy` — Subdir + `Dir[Write]` actor

Worked, but the file only had the *last* line written — see Finding #4
(write-is-replace, not append; book comment is misleading).

### `12_tests.witchy` — `witchy test`

All five test functions passed. `assert_int_eq` and `assert_eq` (strings) both
worked. **Worked-well.**

### `13_inventory.witchy` — JSON, `impl Show`, `impl Item: fn total(self)`, dict

First attempt failed because I used `match arm -> return Err("...")` inline —
that's Finding #3. I refactored two `match` blocks into named helpers
(`as_array_or_err`, `item_or_err`) that returned `Result`, and chained with `?`.
Worked after that. The `dict.get_or(t, key, 0) + 1` upsert pattern is a clean
witchy idiom.

### `14_event_summary.witchy` — Capstone: parse log → tally → comprehension-filter → render

First attempt failed because I used `match arm -> out = list.push(...)` — same
Finding #3. Refactored to `if let Some(e) = ...`. Worked. `[e for e in events
if e.severity == "ERROR"]` (Finding #2 — comprehensions) was the cleanest line
in the whole program.

### Package manager — `mylib` ↔ `myapp` path dep

- `witchy new mylib`, edited to a library (deleted scaffolded `main`),
  `witchy build` says `(library — no main; import it from another rune)` —
  helpful.
- `witchy new myapp`, `witchy add mylib --path ../mylib` — worked,
  edited the consumer to `import mylib` and called `mylib.shout("hello")`.
- `witchy audit` and `witchy tree` and `witchy why mylib` (had to `cd` —
  Finding #10).
- Probed the widening gate: I added a `Net[Connect, Tcp]` fn to mylib;
  `witchy update` blocked with a clean message:

  ```
  BLOCKED: this change would widen your dependency tree's capability footprint.
    + Net[Connect, Tcp]  (runtime) introduced by: mylib
    To accept, re-run:  witchy update --allow-cap Net[Connect, Tcp]
  ```

  Then reverted and re-ran. **Worked-well.**

### Build steps — `genlib` (with `build.witchy`) ↔ `genapp`

I went down a rabbit hole here (Finding #11). My first model was that
`build.witchy`'s emitted source would be folded into the host rune's module
(so `genlib.outer()` could call a generated `generated_message()`). Wrong:
the build output is a *separate* module the consumer imports. After reading
`tests/e2e.rs` I rebuilt it correctly:

- `src/build.witchy`: `fn build(out: BuildOut): write_out(out, "generated.witchy", "pub fn generated_message() -> String:\n    \"hello from a build step!\"\n")`
- `src/genlib.witchy`: just `pub fn id(s: String) -> String: s`
- `genapp/src/genapp.witchy`: `import generated` (not `import genlib`!) and
  `print(console, genlib.id(generated.generated_message()))`.
- The widening gate fired on `add`:
  `BLOCKED: ... + BuildOut (build) introduced by: genlib`. Re-ran with
  `--allow-build-cap BuildOut`.
- A second gate fired on `build`:
  `genlib ships a build step, and build-time code execution is denied by
  default`. Added an empty `[build.grants."genlib"]` section to accept.
- `witchy run` then ran the build step and printed
  `hello from a build step!`. **Worked-well.**

The two-layer gate (kind-widening + execution-consent) feels right; the book
explains both layers; the only confusion was where the generated source *lives*.

---

## Smaller probes (reproducers)

- `probe_narrow_fail.witchy` — writing through `Dir[Read]` produces
  `\`write\` needs \`Write\` but the capability is \`Dir[Read]\``.
- `probe_net.witchy` — listening on `Net[Connect, Tcp]` produces
  `\`listen\` needs \`Listen\` but the capability is \`Net[Connect, Tcp]\``.
- `probe_return.witchy` — Finding #3 reproducer.
- `probe_string_escape.witchy` / `probe_interp_call.witchy` /
  `probe_interp_quote.witchy` — Finding #6 reproducers.
- `probe_perf.witchy` / `probe_perf2.witchy` — perf-note reproducer.
- `probe_iflet.witchy` — Finding #7 (fmt rewrites if-let to match).

---

## Suggested book additions

Listed in order of how badly I missed them:

1. **A `derive(...)` section** in `tour-generics.md` (Finding #1). Right now
   `derive` exists in the language and works, but a learner would not find it.
2. **A `comptime:` section** — probably its own short page (Finding #1, #2).
3. **A comprehensions paragraph** somewhere prominent — `tour-values.md` or
   `tour-iterators.md` (Finding #2).
4. **A `match` semantics note** that `arm -> body` requires an *expression*;
   for `return` or assignments, use the indented form (Finding #3).
5. **A `Dir` operations table** in the capabilities chapter listing the verbs
   (`read`, `write`, `subdir`, `exists`, `make_dir`, `is_dir`, `list`) and
   their semantics (write = overwrite) (Finding #4).
6. **A "where does generated code go?" subsection** in `packages-build.md`
   (Finding #11). One paragraph would have saved me 20 minutes.

---

## Methodology notes

- Used the freshly-built `target/release/witchy` exclusively, never rebuilt.
- Wrote programs `01..14` in `scratch/`, plus seven `probe_*.witchy` files for
  reproducers.
- Used `witchy fmt` on every main program; reformatted in place.
- Verified each program with `witchy parity` (where the program took no
  command-line args), `witchy check`, and `witchy sandbox` (where applicable).
- Ran the test runner on `12_tests.witchy`.
- Did NOT modify anything outside `scratch/`.

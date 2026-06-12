# Witchy Learning Log

An honest record of an LLM (Claude) learning witchy *from the book only*, then
writing and running real programs in `scratch/`. Each program is written first
from book knowledge; mistakes, surprises, and friction are logged below as signal
for where the docs / language / stdlib could improve.

Method:
- Source of truth while writing: `book/src/**` only.
- I consult `docs/stdlib.md` or `examples/` *only after* hitting an error, and
  note when I had to leave the book.
- Every program is run with `witchy <file>` (and `witchy parity`/`caps` where
  relevant).

Legend for severity:
- 🟥 blocker — I could not proceed without leaving the book / guessing.
- 🟧 friction — cost me a wrong attempt; book implied otherwise or was silent.
- 🟨 papercut — minor surprise, quick recovery.
- 🟩 worked — notable thing the book taught well / first try success.

---

## Programs

### 01_temperature.witchy — records, `Show` trait, `match`, int math
Goal: `Temp` record, custom `Show`, C/F conversion, classification over a list.
Result: ✅ runs, parity passes, `caps` = `Console`.

Written first-try with no language mistakes from book knowledge. Negative integer
literals (`Temp(-5)`), the `Show` impl, and `show.say(console, x)` all worked
exactly as the book describes.

### 02_wordcount.witchy — strings, dict counting, builtins discovery
Goal: tokenize text, count word frequencies in a `Dict(String, Int)`, print them.
Result: ✅ after 2 corrections. Surfaced the builtins-vs-module-functions gap.
Mistakes: `for (word,n) in ...` (no tuple pattern in `for`); `string.split`
(actually builtin `split`); guessed `dict.pairs` (actually builtin `pairs`).

### 03_wordfreq.witchy — sort_by, records, padded report
Goal: rank words by count desc then alpha asc, print an aligned table.
Result: ✅ first try once I'd learned `list.sort_by`. parity passes.
Used a `WordCount` record (not a tuple) specifically because tuple fields aren't
accessible inside the single-expression `less` lambda. The inline
`if a.count != b.count: ... else: ...` worked as an expression in the lambda.

### 04_wc.witchy — capabilities: Dir[Read], args, early return, sandbox
Goal: a `wc`-style CLI over a file argument.
Result: ✅ runs on interpreter AND `witchy sandbox --dir ...`; `caps` =
`Console, Dir[Read]`; confinement rejects `../` paths. `return 1` early-exit from
`main` works. Reads `scratch/data/sample.txt`.

### 06_primes.witchy — generators + lazy iterators (gen fn, filter/take/collect/fold)
Goal: infinite `naturals` generator, filter to primes, take 10, sum them.
Result: ✅ interpreter first try; the generator/iterator model worked exactly as
the book teaches. parity initially failed twice on F11 (interpolating a
`collect`ed list and a `fold`ed Int); fixed with a manual `show_ints` helper +
`int_to_string`. Now parity passes. (This is *why* the book's own iterator
examples hand-roll a `show(xs)` helper instead of `${xs}`.)

### 09 greeter/ — package manager scaffold (`witchy new`)
Goal: scaffold and run a rune.
Result: ⚠️ partial. `witchy new scratch/greeter` created the dir at `./greeter`
(basename, in cwd) and set `name = "scratch/greeter"` (slash and all) in
`witchy.toml` and the source string (F17). I moved it under `scratch/` and fixed
the name. Could not drive `witchy build`/`run`: they only read `./witchy.toml`
from cwd with no path/manifest flag (F18), and I can't `cd` into the rune in this
environment. The rune's source runs directly (`witchy scratch/greeter/src/...`).

### 07_actor_calc.witchy — actors with multiple handlers + mutable state
Goal: a `Calculator` actor handling `Add`/`Sub`/`Report`.
Result: ✅ first try from the book. `caps` reports the actor's footprint. Note:
message constructors (`Add`, `Report`) are never declared as a `type` — the `on`
handlers define them implicitly. Worked, but the book never states this.

### 08_config.witchy — Result, `?`, custom error enum; uncovered a stdlib bug
Goal: parse `key=value` lines into a `Dict`, look up required keys, with `?`
chaining and a `ConfigError` enum.
Result: ran correctly on the interpreter; `witchy parity` then revealed a true
**divergence** (not a compile failure) traced to `dict.get` (F15). After
switching to the builtin `get_or`, parity passes. `?`, custom-error `Result`
propagation, and `split_once`'s tuple return all worked correctly.

### 05_json.witchy — json encode/decode; uncovered a WASM codegen bug
Goal: build a `Json` value, encode it, decode a string back, re-encode.
Result: ✅ on interpreter first try (my `JsonObject/JsonString/JsonInt`
constructors were correct!). One naming miss (`json.parse` → `json.decode`). Then
`witchy parity` exposed a real WASM codegen bug (F11) on `"...: ${e}"`; switched
to `"...: " <> e` and parity passes.

---

## Findings

### Summary / prioritized (most actionable first)
1. **F15 🟥 `dict.get` silently returns wrong answers on the compiled backend** for
   runtime-built keys (pointer vs content equality). Wrong results on the
   sandbox, no error — the exact thing parity exists to forbid. Fix the stdlib to
   use `Eq`; add a runtime-built-key parity test.
2. **F11 🟥 `to_string`/`${}` fails WASM codegen** for values whose type comes from
   monomorphization (ADT payloads, some generic returns like `iter.collect`/
   `fold`). Runs in dev, breaks in sandbox. Inconsistent (some Ints survive).
3. **F5 🟥 Builtins vs module functions is undocumented** — the single biggest
   *learnability* gap; you can't guess `split(...)` vs `string.split(...)`.
4. **F16/F2/F3** correctness/setup papercuts that produce confusing errors
   (Option import inconsistency; stale embedded-stdlib link errors; macOS copy
   SIGKILL).
5. **F4,F6,F7,F8,F9,F12,F13,F17,F18** doc/ergonomic gaps with easy fixes.
6. **F1,F14,F19 🟩** what worked great: the core language, generators/iterators,
   actors, capabilities all translated from book to working code on first try.


### F1 🟩 Book→program worked first try for a non-trivial program
`01` used records, a custom `Show` impl, `show.say`, `match`/`if`-expression
chains, integer division semantics, string interpolation of a record field, and
`for ... in list` — all correct on the first attempt straight from the book.
Negative literals (`-5`) work even though every *prose* example in the book
avoids unary minus (`0 - 2`); the appendix literal table (`-7`) is the accurate
one. (Minor doc smell: prose vs appendix disagree on whether `-5` is idiomatic;
harmless but could confuse.)

### F2 🟥 (test-harness, not language) Stdlib is embedded; a stale binary lies
The stdlib is compiled into the `witchy` binary via `include_str!("../std/*.witchy")`
(see `src/linker.rs`). My first run failed with `link error: module show has no
function say` purely because the on-PATH binary predated `say` being added to
`std/show.witchy`. Implication for *consumers*: if someone installs an older
`witchy` but reads current book/docs, stdlib functions silently "don't exist"
with a confusing low-level `link error`. Worth considering: a version stamp +
"this function was added in vX" or a clearer "unknown function `show.say`" error.

### F3 🟥 (macOS packaging) Copying the binary makes it SIGKILL (Gatekeeper)
`cp target/release/witchy ~/.cargo/bin/witchy` produced a binary that macOS kills
with signal 9 (exit 137, no output) — copying an ad-hoc-signed Mach-O invalidates
its signature. A **symlink** works. This bit me twice (CLI + the Zed LSP binary).
The book's install section says `cargo install --path .` / `cargo build --release`
which is fine, but anyone who *copies* a prebuilt binary on macOS hits a silent
kill. Worth a one-line note in `getting-started-installation.md`.

### F4 🟧 No tuple pattern in `for` — `for (k, v) in pairs(d)` is a parse error
Coming from Python/Rust I wrote `for (word, n) in dict_pairs`. Parse error
(`expected an identifier, found "("`). The required idiom is two steps:
```
for entry in pairs(d):
    let (k, v) = entry
```
This is *confirmed as the intended idiom* — `std/dict.witchy` itself writes exactly
that in every function. But the book never shows a `for` over pairs, so a newcomer
will almost certainly try the destructuring form first. Either support tuple
patterns in `for`, or show this idiom in `tour-functions.md`/`tour-values.md`.

### F5 🟥 Builtins vs module functions is undocumented — the biggest gap so far
The book/`appendix-stdlib.md` lists e.g. `split` under the `string` module, but
`split` is a **builtin** called *unqualified* (`split(s, " ")`); `string.split`
is a link error. Lots of core ops are builtins: `split`, `contains`, `to_lower`,
`to_upper`(?), `substring`, `index_of`, `replace`, `starts_with`, `ends_with`,
`string_length`, `char_count`, `string_chars`, plus `dict_new`, `insert`,
`get_or`, `pairs`, `size`, `push`, `at`, `length`. Nothing in the book
*enumerates the builtins* or says "these are called without a module prefix." As
an LLM I could only discover them by reading `std/*.witchy` source and seeing
which names they call unqualified. Recommendation: a "Builtins" reference page
(or a clear marker in the stdlib appendix: builtin vs `module.fn`), because the
qualified-vs-unqualified choice is not guessable and the error (`module X has no
function Y`) doesn't hint that Y is actually a builtin.

### F6 🟧 Dict iteration is underspecified in the book
`appendix-stdlib` / `tour-functions` say `for ... in` walks "a dict's views" but
never show how to get keys/values/pairs. The answer is builtins `pairs(d)`,
`size(d)` (and `dict.get`, `dict.from_pairs`, ... in the module). I had to read
`std/dict.witchy`. A one-line recipe ("iterate a dict with `for e in pairs(d):
let (k,v) = e`") would remove this entirely.

### F7 🟧 Sorting API needs a signature in the docs
The appendix lists `sort` for both `list` and `ord` with no signatures. Reality:
`list.sort_by(xs, less: fn(a,a)->Bool)`, `list.sort(xs: List(Int))`,
`ord.sort(xs) where a: Ord`. The `less`-predicate shape (not a key function, not
an `Ord.compare`-style Int) is a specific, non-obvious choice. A consumer sorting
by a derived key will want either `sort_by`-with-key or to know to write a `<`
lambda. Worth one concrete example in the book (the tour mentions `ord.max` but
never shows sorting).

### F8 🟧 No tuple indexing (`t.0`/`t.1`) — forces records for pair comparators
`pair.0` is a parse error (`expected an identifier, found "0"`). Tuples are
destructure-only. Combined with single-expression lambdas, you *cannot* compare
tuple fields inside a `sort_by(..., fn(a,b): ...)` — there's no way to name `a`'s
components. I worked around it by switching from `(String, Int)` pairs to a
`WordCount` record (records have `.field` access). This is a real ergonomic
cliff: the obvious "sort a list of pairs by the second element" has no direct
expression. Options: allow tuple indexing, or allow multi-statement/`let` in
lambda bodies, or document "use a record when you need to compare components."

### F9 🟧 How to run a capability program on the *interpreter* is undocumented
The book only ever runs Dir/args programs via `witchy sandbox --dir X prog a b`.
It never says what plain `witchy prog.witchy <args>` does. Empirically: trailing
args become `args: List(String)`, and a `Dir` parameter is backed by the current
working directory (so `witchy scratch/04_wc.witchy sample.txt` looked for
`sample.txt` under the repo root, not next to the file). That cwd-rooting is
reasonable but surprising and unstated. Add a recipe: "running a program that
takes a `Dir`/args on the interpreter" alongside the `sandbox` examples.

### F10 🟨 `string.lines("a\n")` yields `["a", ""]` (trailing empty line)
My `wc` reported 4 lines for a 3-line file with a trailing newline, because
`lines` is `split(text, "\n")` and the final `\n` produces an empty trailing
element. Defensible, but differs from Unix `wc`/most expectations. Worth a doc
note on `lines`, or a `lines` that drops a single trailing empty.

### F11 🟥 WASM codegen bug: `to_string`/`${}` on a String bound from an ADT variant
The headline finding. A program can run perfectly on the interpreter but **fail
to compile to WASM** (so it can't be sandboxed) with:
```
codegen error: to_string could not determine the value's type for WASM;
convert it explicitly (e.g. int_to_string) or implement `Show`
```
Isolated minimal repro (`probe_enum_str.witchy`):
```
type Msg:
    Text(String)
    Empty
fn main(console: Console):
    match Text("hi"):
        Text(s) -> print(console, "got: ${s}")   // <- fails WASM codegen
        Empty -> print(console, "none")
```
Characterization (each tested via `witchy parity`):
- `${s}` where `s: String` bound by an **ADT variant pattern** (`Text(s)`,
  `Err(e)`) → **FAILS** on WASM. (Both generic `Result` and a plain user enum.)
- `${v}` where `v: Int` bound by the same kind of pattern (`Ok(v)`) → works.
- `${s}` where `s: String` is a plain local, a tuple-destructured binding, or a
  record field → works.
So the type of a **String** payload bound from an enum variant is not propagated
to the `to_string` lowering in the WASM backend (Int is). Workaround for users:
concatenate instead of interpolate (`"got: " <> s`) since `to_string` on a String
is identity — that compiles. Impact: this is easy to hit (printing an `Err`
string is the single most common thing you do with a `Result`), the program runs
fine in dev, and only `parity`/`sandbox` reveals it. The error message is good
(it names the fix direction) but doesn't say *which* value or that `<>` works.

**Broader characterization (after program 06).** It is not limited to String, and
not cleanly "generic returns." More `parity`-tested data points:
- `${collect(...)}` (a `List(Int)` from `iter.collect`) → FAILS; a **literal**
  `[1,2,3]` interpolated → works. So the book's claim that `${xs}` is cross-backend
  is only true for directly-typed lists, not ones returned by generic combinators.
- `${fold(...)}` (an `Int` from `iter.fold`) → FAILS.
- `${id(5)}` (an `Int` from a user generic `fn id(x:a)->a`) → works.
- `${v}` (`Int` from `Ok(v)`) → works; `${e}` (`String` from `Err(e)`) → fails.
So the concrete type recovered at the `to_string` lowering site is **leaky and
inconsistent** across monomorphized sources — some stdlib generics (`collect`,
`fold`) and all ADT-String payloads lose it; a trivial generic and ADT-Int
payloads keep it. Reliable user rule: never interpolate a value that isn't a
literal/param/record-field/builtin-typed local; convert with `int_to_string` /
a manual `show` / `<>`. (And see F13 — you can't annotate the local to help.)

### F12 🟨 JSON: `decode` not `parse`; and there is no `JsonFloat`
Two notes from `std/json.witchy`: (1) the appendix says JSON "parse and encode";
the function is `json.decode(s) -> Result(Json, String)` (and `encode`/
`encode_pretty`). Naming the doc verb "parse" but the function `decode` cost me a
wrong guess. (2) The `Json` type has `JsonInt(Int)` but **no `JsonFloat`** — JSON
numbers with a fractional part aren't representable. Fine for many uses but a
landing mine for anyone round-tripping real-world JSON; should be documented (or
added).

### F13 🟨 No type annotations on locals — blocks the natural F11 workaround
`let xs: List(Int) = iter.collect(...)` is a parse error (`expected "=", found
":"`). The book does say "locals are inferred," so this is intentional, but it has
a sharp interaction with F11: the obvious fix — "tell the compiler the type so
`to_string` can find it" — is unavailable, leaving only manual conversion. If the
F11 root cause is hard to fix, allowing a local annotation that the WASM codegen
consults would at least give users an escape hatch.

### F15 🟥 CRITICAL: `dict.get` silently diverges on the compiled backend (pointer eq)
The most serious finding. `std/dict.witchy`'s `get` (and `has_key`, `merge`,
`invert`, `from_pairs`-collision, ...) compares keys with `k == key` where `k` is
a **type variable**. Per `eq.witchy`'s own warning, generic `==` on a
type-variable value is **pointer comparison in compiled code**. So on the WASM /
sandbox backend, `dict.get` fails to find a key whose stored form is a
*runtime-built* string (anything from `trim`, `split`, concatenation, file input,
JSON, ...) even though the content matches.

Minimal repro (`probe_dictget_runtime.witchy`), via `witchy parity`:
```
let k = string.trim("  host  ")   // runtime-built "host"
insert(d, k, "localhost")
dict.get(d, "host")               // literal lookup
// interpreter -> Some("localhost");  compiled -> None
```
It is masked when both the stored and lookup keys are identical *literals*
(interned to one pointer), which is why my first probe falsely passed and why
casual examples won't catch it. It bites the moment keys are computed — i.e.
every real config/JSON/CSV parser. This silently produces **wrong answers** on the
sandbox (no crash, no diagnostic), which is the one thing the parity discipline
exists to forbid.

Fix direction: `dict.get` et al. should require `where k: Eq` and use `eq(k,key)`
(the std already *has* `Eq` exactly for this), or the builtin dict should back
`get`. Until then, the safe API is the **builtin** `get_or` (Rust-side content
equality). Recommend: (a) fix the stdlib functions, and (b) add a property/parity
test that inserts runtime-built keys, since literal-key tests pass vacuously.
Note the trap is general: any user code doing `==` / `eq.member` / `list.contains`
on type-variable values of a non-primitive type has the same compiled-vs-interp
split. The book mentions this for `==` in passing (tour-values says `==` is
structural) but `eq.witchy`'s caveat isn't surfaced in the book at all.

### F16 🟧 Inconsistent `unknown constructor Some in pattern` WASM codegen error
Matching `Some(v)`/`None` on a `dict.get` result failed WASM codegen with
`unknown constructor Some in pattern` until I added `import option` — yet
program 08 matched `Some`/`None` (also without `import option`) and compiled
fine. So the requirement to `import option` for Option constructors to exist in
the WASM backend is *inconsistent* (interpreter never needs it). Either Option/
Result constructors should always be available to codegen (they're built-in
enums), or the checker should require the import uniformly on both backends. As
an LLM this was confusing because the *same* pattern compiled in one file and not
another.

### F14 🟩 Generators & lazy iterators just worked
`gen fn` + `while true` + `yield`, then `iter.filter/take/collect/fold` with
lambda predicates — all correct on both backends on the first try from the book.
This chapter of the book is high quality and translated directly into working
code. (Only the printing of results tripped on F11, not the iteration itself.)

### F17 🟧 `witchy new <arg>` mishandles a path-like name
`witchy new scratch/greeter` (1) created the directory at `./greeter` (the
basename) in the current dir rather than at `scratch/greeter`, and (2) wrote
`name = "scratch/greeter"` — a rune name containing `/` — into `witchy.toml` *and*
the generated source string. Either accept only a bare name (and reject `/`), or
treat the arg as a path and create there with `name = basename`. As-is it's easy
to end up with an invalid rune name and a directory you didn't expect.

### F18 🟧 `witchy build`/`run` are cwd-only — no `--manifest`/path option
They hard-read `./witchy.toml` and ignore a directory argument
(`witchy build scratch/greeter` → `cannot read ./witchy.toml`). The book only ever
shows `cd <rune> && witchy run`. Without a `--manifest <path>` / project-dir
argument, these commands are hard to drive from CI, editor tooling, or an agent
that can't freely `cd` — and inconsistent with `run/check/parity/sandbox`, which
all take a file path. A `-C <dir>` or `--manifest` flag would fix it.

### F19 🟩 `caps`, `parity`, `sandbox`, and the error discipline are excellent
The tooling did its job repeatedly: `caps` gave exact per-function footprints;
`sandbox` printed the precise grant and enforced confinement; and `parity` is the
hero — it surfaced BOTH serious bugs (F11, F15) that the interpreter happily hid.
The "runs in dev, only parity/sandbox reveals it" pattern shows the parity check
is doing exactly what it's designed to, and any consumer shipping to the sandbox
should treat `witchy parity` as mandatory CI. (Meta-point for docs: the book
could say "run `parity` before you trust a program in the sandbox" more loudly.)

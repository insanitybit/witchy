# Witchy Learning Log

An LLM (Claude Opus 4.7) learns witchy by reading book/, then writing programs
to scratch/. Each program is fmt'd; each non-actor-or-Net program is parity'd.

Format of entries: short tag + verbatim error (when present) + expectation +
fix + severity (Blocker / Friction / Papercut / Worked-well).

---

## Probes (before the named programs)

### probe_string_concat: `+` works on strings

- Wrote `print(console, "hello, " + "world")`. Worked.
- Worked-well.

### probe_string_concat2: `<>` is rejected with a helpful message

- Wrote `print(console, a <> b)`.
- Error: `type error: \`main\`, line 4: \`<>\` was removed — \`+\` concatenates strings, and \`"${a}${b}"\` interpolates`
- Expected: book uses `+`; some older docs/search hits mention `<>`.
- Fix: use `+` (or interpolation). The error message is excellent (Worked-well).
- Severity: Worked-well — verifies the in-progress migration mentioned in the
  loaded memory file `feedback_break_dont_deprecate` is actually executed: the
  compiler kept a helpful "this was removed" diagnostic but the runtime is fully
  the new way. **No book/test inconsistency found** in the actual `book/src`.

### probe `witchy which list.push`: dotted name not accepted

- `witchy which list.push` → `no std function matches \`list.push\``.
- `witchy which push` → finds `list.push(xs, x)` fine.
- Severity: Papercut. The help banner says `witchy which <name>` and the
  function rendered name includes the module prefix, so a user copying the
  rendered identifier back into `which` will be told it doesn't exist.

---

(programs follow…)

---

## 06_strings: heavy interpolation + concatenation + escapes

- Mostly first-try; runs+parity ok after one edit.
- **`witchy fmt` rejects `"\${...}"` (the `\$` escape):**
  - Verbatim: `witchy fmt: cannot format \`scratch/probe_fmt_break5.witchy\` (parse error or unsupported construct)`
  - The same program type-checks and runs fine. The `\$` escape is documented
    in `appendix-operators.md` (escapes: `\n \t \r \0 \\ \" \$`).
  - Severity: **Friction** (formatter disagrees with the compiler on a
    documented escape; not a behavior change, but a documented form is
    un-fmt-able). Worked around by writing `$not_interpolated` without `${`.
- `witchy fmt` also unwraps parens around an interpolated string in a list
  comprehension (`["${n}" for n in xs]`). Runs/parity identical → Worked-well.

## 07_generics_traits: generics + traits + derive(Show,Eq,Ord) + impl Show
First-try; parity ✓. Worked-well.

## 08_json_derive: derive(Json), json.encode/decode round-trip
First-try; parity ✓. fmt collapsed multi-line ctor calls onto one line.
Worked-well.

## 09_comptime: emit() in a comptime: block
First-try; parity ✓. Worked-well.

## 10_iterators: iter.range/filter/map/take/collect + gen fn + collatz/fibs
First-try; parity ✓. Worked-well.

## 11_large_iter: 5000-elt range + an infinite gen + take_while + primes<2000
First-try; parity ✓ (303 primes, 20833332500). Worked-well at scale.

## 12_scan: log scanner with Console+Dir[Read]+Env+args, sandboxed
First-try; sandbox runs; `caps` reports `Console, Dir[Read], Env`; `..`
rejected at the Dir boundary. Worked-well.

## 13_narrowing: `as`, retain console, without dir, dropped names
- First try blew up on `string.slice` — that function does not exist; the
  string module has `substring(s, start, end)`. **Papercut**: I picked the
  most-common-name-from-other-languages name. `witchy which slice` says
  "list.slice" which is the trail.
- Also wrote `let _ = m` as an "ignore" line; fmt rewrote it to `m`. That
  changes the AST (a bare expression statement) but didn't change behavior.
  Papercut — feels unusual but defensible.
- Otherwise sandbox runs; firewalls work. Worked-well.

## 14_actors: spawn-supplied state, capability passing, Boss/Worker
First-try; parity ✓. fmt collapsed empty lines between `on` handlers.
Worked-well.

## 15_time + 15_time_pure: time / duration round-trips
- **Friction (bordering on silent-wrong):** `now(clock)` returns
  MILLISECONDS, but `time.from_unix(secs)` takes SECONDS. Passing
  `now(clock)` straight into `time.from_unix` produced *no error* and a
  garbage date in the year 58416. The book mentions Duration is "carried as
  whole milliseconds" but the `Clock`/`now` interface isn't documented in
  the book; the `time` doc says "secs since the unix epoch". A user who
  follows the obvious naming will write the bug I wrote.
- **Friction**: `let timeout: Duration = 1m + 30s` is a parse error:
  `expected \`=\`, found \`:\``. Type annotations on let bindings aren't
  supported (the book says "locals are inferred"), but the message doesn't
  say that; it complains about the `=` it expected to find before the `:`.
- **Friction**: bare `say(console, x)` fails with "did you forget
  `import show`?" — perfect message; **Worked-well** half of the friction.
- Once split, the pure half passes parity ✓.

## 16_tests: `witchy test` over `test_*` functions
Worked-well first-try; 3 passed.

## 17_multi-rune (wordlib + wordapp via path dep, CLI args)
- **Friction**: I wrote `c == " " or c == "\t"` from muscle memory. Error:
  `type error: unbound variable \`or\``. The witchy operator is `||`. Good
  error, but "or"/"and" being unbound rather than "use ||" is a Papercut.
- After fmt'ing wordlib, `build` refused with `path dependency \`wordlib\`
  changed: hash ... — run \`witchy update\``. **Worked-well** — exactly the
  intended guard (fmt actually changed bytes), and the message says exactly
  what to do.
- `string.lines("a\nb\nc\n")` yields 4 entries (one trailing empty). That's
  legitimate but surprised me. Papercut.
- `witchy run -- count data/sample.txt` works; `witchy sandbox` on the entry
  file directly does NOT follow project dependencies (it can't find
  imported `wordlib`). **Friction** — sandbox is single-file only.
- `caps` reports `Console, Dir[Read]` for the project: clean.

## 02_pure_math: pure functions + classify + factorial + fib

- Worked first-try; parity ✓.
- fmt removed an empty line between a top-level `//` comment and the first
  `fn`. This is fine but a Papercut: it means a "header comment" right above
  a function gets glued onto it. (No behavior change.)
- Severity: Worked-well.

## 03_data_types: records, enums, match, list-rest patterns, guards

First-try; parity ✓; Worked-well.

## 04_errors_values: Option, Result, `?`, `if let`

First-try; parity ✓; Worked-well.

## 05_collections: lists, tuples, dicts, comprehensions, structural eq

First-try; parity ✓; Worked-well. The interpolation rendering of nested
compound values (`${xss}` → `[[1, 2], [3, 4], [5]]`) is identical on both
backends, exactly as the book promises.

## 18_ownership: inout/var, let-borrow, own/move

First-try; parity ✓. fmt rewrote `inout n` to `var n` (the book lists them
as synonyms). Worked-well.

## 19_cap_optional: Option(Dir[Write]), capability enum, narrowing in sandbox

First-try; sandbox executed it; `caps` correctly aggregated `Dir` from the
`Access` enum and `Dir[Write]` from `record`. Worked-well.

## 20_net_narrow: a function whose signature is `Net[Connect, Tcp]`

- First try: error `in call to \`list.length\`: expected \`List(?)\`, found
  \`String\`` — I wrote `list.length(host)` thinking length-is-length. The
  error message is genuinely excellent (Worked-well).
- After fixing: `caps` correctly reports `Net[Connect, Tcp]`; parity ✓.

## 21_collections_deep: set/dict/string/sort

First-try; parity ✓. Note: `string.split("hello, witchy world", ", ")` =
`[hello, witchy world]` (one element) because the input only contained ONE
", "; that's correct, I miscounted.

---

# Summary

Total programs written: **17 named programs + 1 multi-rune project (wordlib +
wordapp) + 7 probes** = 25 .witchy files in scratch/.

**First-try successes** (no edits between Write and a green run): 13 of 17
named programs. Edits needed for: 06 (`\$` fmt-rejection workaround), 13
(`string.slice` → `string.substring`), 15 (let-type annotation parse + `say`
needs `import show` + `now`/`from_unix` ms-vs-s mismatch), 17 (the wordlib
function `count_words` used `or` instead of `||`), 20 (`list.length(host)`
typo on a `String`).

**Blockers**: 0. fmt is idempotent across every file I wrote, fmt --check is
clean, and no post-fmt program changed its observable output.

**Silent-wrong findings**: 1 candidate (not strictly a Blocker, but the
sharpest finding I made):

- `now(clock)` returns MILLISECONDS, `time.from_unix(secs)` takes SECONDS.
  Passing the former straight into the latter produces a date in the year
  58416 with no error. The book never spells out the unit of `now`, and the
  `time` doc explicitly says "seconds." A `Duration` round-trip via
  `int_to_duration` works as expected; only the cross with `time.from_unix`
  produces the wrong-not-loud answer.

## Top friction findings (sorted)

1. **`witchy fmt` chokes on a documented string escape (`\$`)** — the same
   program type-checks, runs, and parity-passes; fmt prints
   `(parse error or unsupported construct)` with no line/column.
2. **`now(clock)` ms vs `time.from_unix` seconds** — silently wrong cross
   between two prelude-grade modules.
3. **No `or`/`and` keywords** — `||` and `&&` are the operators; the error is
   "unbound variable `or`" rather than "use `||`". Universal newcomer
   stumble.
4. **Type annotations on `let` don't parse** — `let x: Duration = 1m` fails
   with `expected '=', found ':'`. The book says locals are inferred, but a
   user *can* read the parameter-annotation style and try it on `let`.
5. **`sandbox` is single-file only** — does not follow project dependencies,
   so a multi-rune project can be `run` but cannot be `sandbox`ed without
   producing a single-file build first. `witchy run` is the substitute, but
   the boundary isn't physical that way.

## Things that genuinely impressed me

- **fmt as a rewrite vehicle**: it canonicalizes meaningful forms (escaped
  quotes inside `${...}` become bare, single-statement match arms collapse,
  parenthesized one-element comprehensions lose their parens), and was
  idempotent on every program I wrote. The "fmt changed the bytes →
  `witchy build` refuses with `path dependency changed`" loop is exactly the
  intended interaction.
- **Diagnostics**: nearly every error I hit pointed me at the fix. The
  `<>` removal message, the `say` / `import show` suggestion, the
  `expected List(?), found String`, even the path-escape `..` refusal —
  every one of them is a sentence I could fix from.
- **Parity actually holds** across heavy iterators, generators (including
  infinite ones), comptime, actors with state and spawn-argument passing,
  and structural equality on nested compounds. The promise that "the
  sandbox runs what you tested" feels real.

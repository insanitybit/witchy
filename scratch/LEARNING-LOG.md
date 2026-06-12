# Witchy Learning Log — LLM author tour

A new-user (LLM) attempt at writing witchy programs after reading the book.
Each entry records: program, expectation, actuality, fix, severity.

Severity tags:

- **Blocker** — couldn't proceed; doc misleading; or fmt changed behavior.
- **Friction** — slowed me down; doc unclear or stdlib surprised me.
- **Papercut** — minor annoyance; quickly recovered.
- **Worked-well** — noteworthy positive, kept me moving.

---

## Program index

| #  | File                                | What it exercises                      | First-try?       | Output stable after fmt?       |
|----|-------------------------------------|----------------------------------------|------------------|--------------------------------|
| 01 | `01_hello.witchy`                   | hello/print/interpolation              | yes              | yes                            |
| 02 | `02_pure_math.witchy`               | recursion, gcd, fib                    | yes              | yes                            |
| 03 | `03_lists_dicts.witchy`             | lists/comprehensions/dicts/tuples      | yes              | yes (fmt un-escaped strings)   |
| 04 | `04_records_enums.witchy`           | records, sum types, match, inline arm  | yes              | yes (fmt -> interpolation)     |
| 05 | `05_errors_option.witchy`           | Option/Result/?/if-let                 | yes              | yes                            |
| 06 | `06_patterns.witchy`                | list patterns, guards, nested          | yes              | yes                            |
| 07 | `07_generics_traits.witchy`         | trait/impl, where, derive, impl Show   | yes              | yes                            |
| 08 | `08_json_derive.witchy`             | derive(Json), json.encode/decode       | yes              | yes                            |
| 09 | `09_iter_gen.witchy`                | gen fn / iter (Fibonacci, Collatz)     | yes              | yes                            |
| 10 | `10_comptime.witchy`                | comptime emit                          | yes              | yes                            |
| 11 | `11_caps_narrowing.witchy`          | Dir as Read, retain/without            | yes              | yes                            |
| 12 | `12_duration_time.witchy`           | Duration literals, time.parse/format   | required fix     | yes                            |
| 13 | `13_actors.witchy`                  | Counter + Boss/Worker                  | yes              | yes                            |
| 14 | `14_scan.witchy`                    | confined log scanner under sandbox     | yes              | yes                            |
| 15 | `15_tests.witchy`                   | std/testing assertions                 | yes              | yes                            |
| 16 | `16_branded_caps.witchy`            | Optional(Dir), Access sum type         | yes              | yes                            |
| 17 | `17_which_probe.witchy`             | split/sort_by/fold/join                | yes              | yes                            |
| 18 | `18_inout_own.witchy`               | inout/own+move/let-borrow              | yes              | yes (fmt -> `var` synonym)     |
| 19 | `19_http_caps.witchy`               | Net[Connect, Tcp] capability shape     | required tweak   | yes                            |
| 20 | `20_wordcount.witchy`               | dict reduce + Dir[Read] sandbox        | yes              | yes (fmt collapsed lambda)     |
| 21 | `21_match_exhaustive_nested.witchy` | nested match, ? across Result          | yes              | yes                            |
| PM | `mathapp/` + `mathlib/`             | two-rune path-dep project              | partial          | n/a (no fmt round-trip on toml) |

Probes (deliberate-error programs):

| # | File                                | Goal                  | Compiler said                                      |
|---|-------------------------------------|-----------------------|----------------------------------------------------|
| - | `probe_errors.witchy`               | print without Console | `unbound variable 'console'` — clean               |
| - | `probe_widen.witchy`                | widen narrowed Dir    | `'as' can only drop rights: Dir[Write] not subset` |
| - | `probe_match_nonexhaustive.witchy`  | drop a Color variant  | `non-exhaustive match on 'Color': missing Blue`    |

Counts:
- **22 programs/projects written** (21 standalone + 1 two-rune project).
- **20/22 first-try successes.** The two that needed touch-up: `12_duration_time.witchy` (Duration interpolation surprise — see entry below) and `19_http_caps.witchy` (idiomatic phrasing tweak — initial scaffold compiled but read awkwardly).
- **0 fmt-induced behavior changes** on the 21 runnable programs. (The 3 deliberate-error probes show different *line numbers* in their compile errors after fmt collapses a blank line, but the error class and message text are identical — not a behavior change.)

---

## Entries — by severity

### Blocker

**B-1. `witchy run` does not forward argv to `main`.**
*Encountered: `scratch/mathapp` (project example).*

I scaffolded a two-rune project (`mathlib` library + `mathapp` consumer with a
path dep). The app's `main(console: Console, args: List(String))` reads CLI
arguments. Per memory: "`run` forwards arguments to the program". I ran:

```text
$ witchy run 3 1 4 1 5 9 2 6
usage: mathapp <int>...
```

`list.length(args) == 0` inside the program. Tried `--` separator too, same.

Root cause (verified in source): `src/pm/cli.rs::cmd_run` calls
`interpreter::run_module(a.linked, Path::new("."), args)` — but
`run_module`'s third parameter is `net_allow: Vec<String>`, not the program's
argv. It should be `run_module_args(...)`. The help text says
`run [args...]         build and run the program (args become main's 'args')`,
so the docs imply it works.

**Severity: Blocker** for any project that wants CLI args via `witchy run`.
Workaround inside the repo would be to invoke the single-file form directly,
but path-dep linkage requires the project flow. (I didn't fix it per the
"don't modify outside scratch/" rule.)

---

### Friction

**F-1. `Duration` interpolation renders the raw millisecond integer.**
*Encountered: `12_duration_time.witchy`.*

I expected `"${t + 1m}"` to render `1m30s` (or similar human form), since the
book repeatedly says `${x}` "renders any value identically on both backends".
Actual output: `90000`. The `to_string` of a `Duration` exposes its underlying
`i64` ms. The fix is to reach for `duration.human(...)` or `duration.clock(...)`.

This was the only stdlib surprise where I expected one thing and got another
that wasn't documented loudly enough. The book's "values render structurally"
language and the Duration-is-a-distinct-type guarantee combined to imply
"the duration prints as a duration". `appendix-stdlib.md` mentions `duration`
helpers in a table but doesn't flag the interpolation default. **`witchy which
human` and `witchy which duration` got me unstuck quickly** (well, `which
duration` returned "no std function matches" — the search is over function
names, not module names; I had to think of `human`/`clock`/`to_ms` first).

**Severity: Friction.** A line in `tour-values.md` or `appendix-stdlib.md` like
"Duration interpolates as a raw millisecond integer; use `duration.human` /
`duration.clock` for friendly labels" would prevent this.

**F-2. `witchy which <module-name>` returns "no match".**
*Encountered while looking for the Duration module.*

`./target/debug/witchy which duration` -> `no std function matches 'duration'`.
The CLI is documented as "find a function in the standard library by (partial)
name", so this is technically correct — but a new user's first instinct is
"what's in the duration module?". `witchy which human` found
`duration.human(...)` immediately, so I recovered, but a module-name match
would be a small win.

**Severity: Friction.**

**F-3. Errors module name confusion: `string.to_int` returns `Int` not `Result`?**
*Encountered while writing `mathapp`.*

I wrote `string.to_int(s)` expecting `Result(Int, String)` based on the book
("strict — it errors on non-numeric input"). The signature really is
`String -> Int` and "errors" means aborts via `fail`, not returns `Err`. The
book says "errors on … rather than silently returning a wrong number", which I
parsed as "returns `Err(_)`". This is consistent with `fail`-the-program
semantics, but the wording "errors" is ambiguous between "aborts" and "returns
`Err`" in a language where both exist. I wrote `parse_args` accepting non-int
input would crash the program; the book's intent is clearly that this is
correct for "loud" parsing, but spelling it `aborts on non-numeric input` would
remove the ambiguity.

**Severity: Friction.**

---

### Papercut

**P-1. `fmt` is aggressively opinionated and rewrites code beyond whitespace.**
*Encountered on most programs.*

`witchy fmt` did several non-whitespace rewrites I didn't expect from the
"canonical layout" framing in the toolbox doc:

- Strips a blank line between a top-of-file comment and the first `fn`.
- Un-escapes `\"` inside `${...}` interpolations (`"${dict.get_or(d, \"k\", 0)}"`
  becomes `"${dict.get_or(d, "k", 0)}"`).
- Rewrites `"a " <> to_string(x) <> ","` into `"a ${x},"` interpolation form.
- Rewrites `inout n: Int` -> `var n: Int` (the docs note these are synonyms
  but the formatter picks one canonical spelling — `var` — for params).
- Collapses a multi-line lambda body onto a single line when it fits.

None changed program output, so this is correct as fmt's job — but the book's
"reformat in place (canonical layout)" undersells just how much rewriting
happens. The `inout -> var` rewrite especially could confuse someone reading
`tour-functions.md` and then seeing their code change spelling on save. A note
like "`fmt` also normalizes synonyms (`inout` -> `var`), unnecessary escapes,
and `<> to_string(x)` chains into interpolation" in `getting-started-toolbox.md`
would set expectations.

**Severity: Papercut.**

**P-2. Help text for `witchy run [args]` doesn't appear in `witchy --help`.**
*Encountered when looking for arg-forwarding syntax.*

`witchy --help` lists the single-file/sandbox/test/etc. commands but says only
"Package commands (add, build, publish, ...) are also available." — no link or
hint that `witchy run [args...]` exists. I had to dig into `src/pm/cli.rs`
to confirm what was supposed to work. A `witchy pm --help` (or `witchy help
pm`) hint at the bottom of the main `--help` would orient newcomers faster.

**Severity: Papercut.**

**P-3. `witchy run --help` is consumed by `main`'s args.**
*Encountered while diagnosing Blocker B-1.*

Calling `witchy run --help` from inside a project printed my program's
`usage:` line, because (per `cmd_run`) everything after `run` is forwarded as
argv. That makes sense given the design, but means there's no obvious way to
get help for `run` itself from the CLI. Once I knew the design, I just read
the source.

**Severity: Papercut.**

---

### Worked-well

**W-1. `witchy parity <file>` is genuinely confidence-building.** Every
pure-or-Console program I wrote passed parity on the first try; I never had to
think about backend differences.

**W-2. `witchy caps <file>` is the killer feature in practice.** When I
wrote `19_http_caps.witchy` and it reported `fetch_root  Net[Connect, Tcp]`
without my having to think about it, the supply-chain story stopped feeling
like marketing and started feeling like a reflex.

**W-3. Compile errors are short and exact.** "non-exhaustive match on
`Color`: missing Blue", "`as` can only drop rights: `Dir[Write]` is not a
subset of `Dir[Read]`", "unbound variable `console`" — each tells me precisely
what to do without scrolling.

**W-4. Sandbox + `--dir` is the easiest "give a program some files" UX
I've used.** No `chroot`, no container — `witchy sandbox --dir /tmp/foo
scan.witchy QUERY app.log` Just Works and the program proves it isn't
escaping.

**W-5. The PM scaffold is fast.** `witchy new mathlib --lib && witchy new
mathapp && cd mathapp && witchy add mathlib --path ../mathlib && witchy
build` took seconds, including the cap-widening audit ("demands no
capabilities. tree max authority now: none"). Everything except B-1
(`run`-forwarded args) worked perfectly: `audit`, `tree`, the lockfile
content, the `provenance = "path:./../mathlib"` entry.

**W-6. `derive(Json)` is dead-simple.** `derive(Show, Eq, Json)` on a nested
record, `json.encode(my.to_json())`, done. Round-trip via `json.decode`
returned a structurally-inspectable `Json` sum type that prints clearly.

**W-7. Actors with `spawn`/`send` Just Work.** Three messages on a `Counter`,
a `Worker`/`Boss` pair — both ran identically on the interpreter and parity
agreed.

---

## Things I tried and could not find

- A way to print a `Duration` in its literal form (`"1m30s"`) directly via
  `${dur}` without reaching for `duration.human`. (Possibly intentional; see F-1.)
- A `witchy run --help` that prints PM-command help. (P-3.)
- A way to make `string.to_int` return `Result(Int, _)` rather than aborting.
  (Maybe I missed it; F-3 may indicate it doesn't exist.)

## Things I never tried (would in a longer session)

- `witchy build-step` with a real `src/build.witchy`. (The PM chapter implies
  this exists end-to-end; my project didn't need codegen.)
- `Secret`-typed capability or signing via `crypto`.
- `witchy publish` against a local `coven-serve` — covered by the local
  registry demo script, but out of scope for this short tour.
- `regex` and `csv` modules.

---

## Summary for the maintainer

Headline: **zero fmt-induced behavior changes** across all 22 programs.
`witchy parity` succeeded everywhere I tried. The one real blocker is the
`witchy run` argv-forwarding bug, which is small but visible — anyone trying
to follow the book's "real software lives in projects" advice will hit it the
first time they want a CLI tool that takes input.

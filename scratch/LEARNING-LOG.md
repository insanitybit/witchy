# LEARNING-LOG — an LLM learns witchy

> Author: a Claude agent.
> Date: 2026-06-12.
> Approach: read book/src/ end-to-end, then write programs in scratch/, recording every error verbatim, what I expected, what fixed it, and a severity.

Severities used:
- **Blocker** — produces a wrong program or makes a thing impossible
- **Friction** — slowed me down, but worked once I figured it out
- **Papercut** — minor, surprised me briefly
- **Worked-well** — called out because it was unusually pleasant

---

## Programs written

| # | File | One-line |
|---|---|---|
| 1 | `01_hello.witchy` | hello, witchy + interpolation |
| 2 | `02_collections.witchy` | lists, tuples, dicts, comprehensions, fold |
| 3 | `03_shapes.witchy` | records, sum-type enums, exhaustive `match`, guards |
| 4 | `04_errors.witchy` | `Option`, `Result`, `?`, `if let Some` |
| 5 | `05_generics.witchy` | generics, `Ord` bound, `derive(Show, Eq, Ord)`, `impl Show`, `impl Trait` |
| 6 | `06_iterators.witchy` | `iter.range/map/filter/take/collect`, `gen fn` Fibs + Collatz |
| 7 | `07_comptime.witchy` | top-level `comptime:` block emitting fn families |
| 8 | `08_log_scanner.witchy` | the book's project: `Dir[Read] + Env + args`, sandbox-runs |
| 9 | `09_narrowing.witchy` | `subdir(...) as Dir[Write]` + implicit narrowing |
| 10 | `10_actors.witchy` | `Counter` + Boss/Worker actors with `Subject` |
| 11 | `11_tests.witchy` | `witchy test` with `testing.assert_*` |
| 12 | `12_optional_caps.witchy` | `Option(Dir[Write])` and a capability enum |
| 13 | `13_http_client.witchy` | HTTP GET (Net) — see Net-narrowing friction below |
| 14 | `14_clock_duration.witchy` | `Clock` + duration literals |
| 15 | `15_json.witchy` | `json.decode` and accessors |
| 16 | `16_crypto.witchy` | `crypto.sha256` + `encoding` |
| 17 | `17_firewall.witchy` | `without dir:` and `retain:` blocks |
| 18 | `18_caps_diff_{a,b}.witchy` | demonstrates `witchy caps-diff` widening exit code 2 |
| 19 | `19_dice.witchy` | seeded `random.next_below` |
| 20 | `20_actor_caps.witchy` | actor that holds `Dir[Write]` and writes audit lines |
| 21 | `21_regex.witchy` | extract + replace_all |
| 22 | `22_taskq.witchy` | richer record/spread/sort example |
| 23 | `proj/{wordlib,wordapp}` | two-rune project with a path dependency (`witchy run`, `witchy audit`) |

**Probes** (small files that test surprising behavior):
`probe_fmt_compr.witchy`, `probe_fmt_compr_orig.witchy`, `probe_fmt_compr2.witchy`, `probe_regex.witchy`.

**Verification:** every Console-only program verified with `witchy parity`; capability-shaped programs verified by `witchy sandbox --dir <root>`; `caps-diff` and `caps` invoked on relevant programs.

Counts:
- 23 programs (+ 4 small probes + 1 multi-rune project = 28 .witchy files total)
- First-try-success ≈ 16 / 23 ≈ **70%**
- The 7 that needed a fix-up after first run/check are detailed below.

---

## Findings, in order I hit them

### F1. Blocker — `witchy fmt` silently corrupts `let x = [comprehension]`
**Severity: Blocker.**

Minimal repro (`probe_fmt_compr.witchy`):

Before fmt — runs correctly, prints `[1, 4, 9, 16, 25]`:
```witchy
fn main(console: Console):
    let xs = [1, 2, 3, 4, 5]
    let ys = [n * n for n in xs]
    print(console, "${ys}")
```

After `witchy fmt probe_fmt_compr.witchy` the file is:
```witchy
fn main(console: Console):
    let xs = [1, 2, 3, 4, 5]
    let ys = 0
    print(console, "${ys}")
```

Now the same program prints `0`. **The formatter rewrites the program into a wrong one — silently, with no warning.** The interpreter and WASM backend both happily run the corrupted source.

The desugar elsewhere (`probe_fmt_compr2.witchy`, function-tail position) emits a verbose `var __compr0 = []; for n in xs: __compr0 = list.push(__compr0, n * n); __compr0` — at least there it still works, but it exposes synthetic identifiers that shouldn't survive into formatted source.

This violates the docs' explicit claim ("`fmt` preserves your comments and is idempotent", and the testing chapter's "every example is extracted, type-checked, and ... run on both backends"). I expected fmt to be a bijection over well-formed witchy code.

Workaround: don't use a list comprehension as the RHS of a `let`. Either:
- use the function-tail form, or
- inline the loop manually.

---

### F2. Friction — `witchy run` doesn't forward CLI args to the program
**Severity: Friction.**

Repro: `scratch/proj/wordapp/`.

```
witchy run sample.txt
```

prints `usage: wordapp <file>` because `args` in `main(console, root, args)` is empty. Also tried `witchy run -- sample.txt` (same), `witchy run sample.txt extra` (same).

`witchy --help` mentions `[args...]` only for `sandbox`, never for `run`. I worked around it by hard-coding the path in the app.

What I expected: parity with `cargo run -- <args>` or `go run main.go <args>`.

---

### F3. Friction — `Net[Connect, Tcp]` narrowing in user code doesn't compose with `http.get`
**Severity: Friction.**

Repro: `13_http_client.witchy` v1.

```
witchy check 13_http_client.witchy
type error: `13_http_client.fetch`, line 6: in call to `http.get`:
  expected `Net`, found `Net[Connect, Tcp]`
```

The book (capabilities-narrowing.md and appendix-recipes.md) explicitly suggests narrowing a `Net` to `Net[Connect, Tcp]` for an HTTP client. But `std/http.witchy:29` declares `pub fn get(net: Net, ...)` — *not* a narrowed signature. So a downstream function that takes the narrower handle can't pass it to `http.get`.

Either the docs over-promise or the stdlib signature should be `Net[Connect, Tcp]`. Either is correct; the two should agree.

Workaround: accept a full `Net`, with a comment.

---

### F4. Friction — `list.sort_by` is a `<` predicate, not a 3-way compare
**Severity: Friction.**

Repro: my first `wordlib.top` returned `if a.n > b.n: -1 else if a.n < b.n: 1 else: 0` (the C/Java convention).

```
type error: `wordlib.top`, line 33: in call to `list.sort_by`:
  expected `Bool`, found `Int`
```

Once you know `sort_by` wants a `less` predicate the rewrite is a one-liner. The error doesn't say *which* shape it wants, which is the cost.

The stdlib reference appendix lists `list.sort_by` only by name — the actual signature `(xs, less: fn(a,a) -> Bool)` lives in `std/list.witchy`. Surfacing the comparator shape in the appendix (or in the error message) would have shortened this.

---

### F5. Friction — `duration.to_ms` doesn't exist; it's `duration.to_milliseconds`
**Severity: Friction.**

Plus: `duration` is not in the prelude, so you need `import duration`. The book mentions Duration extensively but the only API surface I saw was `1s + 1m`, comparison, etc. The error was cryptic:

```
type error: `main`, line 10: cannot resolve the method call `.to_ms(…)` — methods come from `impl` blocks; a plain function is called as `to_ms(value, …)`
```

The error helpfully redirects toward `to_ms(value, …)` but it's *still* the wrong name. Real name is `duration.to_milliseconds`. The memory note says "readable names" is the policy — that's good, but the appendix should call out the verbose name so users don't guess wrong.

---

### F6. Papercut — JSON: type is `Json`, parser is `decode` (not `JsonValue`/`parse`)
**Severity: Papercut.**

I guessed `json.parse` + `JsonValue` from the appendix line "parse and encode JSON". Real names are `json.decode` / `Json`. The link error is clear (`module 'json' has no function 'parse'`), but the appendix could list one example call.

---

### F7. Papercut — `crypto.sha256` returns a hex *string*, not bytes
**Severity: Papercut.**

I wrapped it in `encoding.hex_encode(h)` and got a 128-char ASCII-hex-of-hex result. The stdlib reference says only "hashing" — surprising for someone with a Rust/Python/Go background where `sha256(...)` returns 32 raw bytes. A docstring like "returns the hex digest" on `pub fn sha256` would catch this.

---

### F8. Friction — tuple `.0`/`.1` field access is a parser error
**Severity: Friction.**

```
let pair = ("a", 1)
let n = pair.1
```

yields:
```
error: wordlib: parse error at 25:14: expected an identifier, found `1`
```

The book on pair destructuring says "destructure". OK, but in a comparator lambda, destructuring isn't natural. (Rust/Swift/Python all support `.0/.1`.) I worked around by using a record. Worth a note in the values tour.

---

### F9. Friction — `without`/`retain` name the *binding*, not the type
**Severity: Friction.**

Wrote `without Dir:`. Got:
```
type error: `main`, line 5: no capability `Dir` is in scope to drop here
```

Real form is `without dir:` (the lowercase parameter name). The error pointed at the binding scope but didn't suggest "use the binding name". A "did you mean `dir`?" hint when there's a parameter `dir: Dir` in scope would shorten this.

---

### F10. Papercut — regex `\d{n}` quantifier not supported
**Severity: Papercut.**

`extract("\\d{3}", text)` returns `[]`. `extract("[0-9]+", text)` works. The stdlib docstring lists `* + ?` but not `{n}` — so the regex engine is honestly K&P-tiny. Fine, but `extract` returning `[]` is silent — I'd appreciate a "this pattern matched nothing" debug aid, or for `{n}` to be a parse error.

---

### F11. Friction — `witchy caps` requires a file argument when run inside a project
**Severity: Friction.**

In a project dir, `witchy build` and `witchy run` know which entry to read. `witchy caps` doesn't:
```
$ witchy caps
usage: witchy caps <file>
```

It would be nice if it inferred `src/<name>.witchy` from `witchy.toml`.

---

### F12. Worked-well — `witchy caps-diff` exit code 2 and clean diff
Probe `18_caps_diff_{a,b}.witchy` worked exactly as documented:
```
WIDENING: the newer version demands new host authority (Net). Review before trusting.
$ echo $?
2
```
This is the single best-designed thing I touched.

---

### F13. Worked-well — sandbox path confinement
A typo'd `../etc/passwd` was rejected with the clean message:
```
`..` escapes the Dir capability
```
Did exactly what the book said.

---

### F14. Worked-well — actors are simple and the WASM-per-actor story is invisible
`10_actors.witchy` and `20_actor_caps.witchy` (with attenuated `subdir(...) as Dir[Write]`) Just Worked. The fact that each actor is its own VM with its own imports is invisible from the writer's perspective, which is the point.

---

### F15. Papercut — `witchy parity` flagged my Clock-dependent program as divergent
Expected: parity is for deterministic programs. `now(clock)` is by definition nondeterministic, so parity says "DIVERGE". The diff output is helpful but the message itself ("✗ ... the two backends DIVERGE") *sounds* alarming for a program that is correct on both backends. A short note "(nondeterministic capability — diverging output is expected)" would help.

---

## Things I tried and got right first try (worth noting)

- All record / enum / match programs (3, 4).
- `derive(Show, Eq, Ord)` with `==`, `ord.max_of` (5).
- `gen fn` + `iter.take`/`collect` for infinite generators — exactly like Rust iterators but with no lifetime gymnastics (6).
- `comptime:` block emitting witchy source for a family of functions (7).
- `Option(Dir[Write])` and a capability enum (12) — the auditor *did* see through them as the docs promise.
- The two-rune project with `path = "../wordlib"` — scaffold, build, audit all worked the first time.

## Top 5 friction findings (one line each)

1. **(BLOCKER)** `witchy fmt` silently rewrites `let x = [..for..]` to `let x = 0` — produces a wrong program from a correct one.
2. **(Friction)** `witchy run` doesn't forward CLI args to the program (no `-- args` shape documented or working).
3. **(Friction)** `Net[Connect, Tcp]` narrowing doesn't pass `http.get`'s signature (`Net`), so the book's recipe doesn't typecheck.
4. **(Friction)** `list.sort_by` takes a `Bool` less-predicate; error doesn't say which shape; stdlib appendix doesn't list signatures.
5. **(Friction)** Naming inconsistencies bit me three times: `duration.to_ms` vs `to_milliseconds`, `json.parse`/`JsonValue` vs `decode`/`Json`, `crypto.sha256` returns hex-string not bytes. A one-line example per stdlib module in the appendix would prevent all of them.

## Other notes

- Zero programs produced silently-wrong output at runtime — every error I hit was either a clear compile-time message or, in one case, the formatter (which is a build-step tool, not a runtime). So **on the language side, there were zero silent-wrong-output findings**; the one silent-wrong-output finding is in `witchy fmt`.
- The `verify-cap-by-reading-the-type` story is real and felt good. `witchy caps` consistently told me the truth.
- `witchy parity` is a comfortable reflex for pure programs.
- `witchy sandbox --dir <root>` is the workflow that made capabilities feel concrete.

use super::*;
use crate::{codegen, interpreter, parser};

    /// (BUG-537) `DateTime`'s public constructors enforce the fixed-width
    /// RFC3339 year domain that `time.iso8601` and `time.parse_iso8601` share.
    #[test]
    fn datetime_rejects_years_outside_fixed_iso_domain_on_both_backends() {
        let src = "import time\n\nfn report(console: Console, r: Result(time.DateTime, time.TimeError)):\n    match r:\n        Ok(d) -> console.print(time.iso8601(d))\n        Err(e) -> console.print(time.time_error_message(e))\n\nfn main(console: Console):\n    report(console, time.civil(0, 1, 1, 0, 0, 0))\n    report(console, time.civil(10000, 1, 1, 0, 0, 0))\n    report(console, time.parse_iso8601(\"0000-01-01T00:00:00Z\"))\n    report(console, time.parse_iso8601(\"9999-12-31T23:59:59Z\"))\n";
        let expected = [
            "year 0 is out of range 1..9999",
            "year 10000 is out of range 1..9999",
            "year 0 is out of range 1..9999",
            "9999-12-31T23:59:59Z",
        ];
        assert_eq!(link_run(src), expected, "interp: DateTime fixed ISO domain");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: DateTime fixed ISO domain",
        );
    }

    /// `fail` is the loud abort on BOTH backends: a runtime error in the
    /// interpreter, a trap in compiled code.
    #[test]
    fn fail_aborts_on_both_backends() {
        let src = "fn main(console: Console):\n    console.print(\"before\")\n    fail(\"boom\")\n    console.print(\"after\")\n";
        let err = interpreter::run(src).expect_err("interpreter must abort");
        assert!(err.message.contains("boom"));
        let module = parser::parse_module(src).expect("parse");
        // `fail()` lowers on the binary path: route the message through
        // `__witchy_abort`, then `unreachable`.
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("fail() lowers on the binary path");
        assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on fail()");
    }

    /// (RFC-0045) The message parity property: when the interpreter aborts, the
    /// compiled backend must abort with the SAME message CORE — not merely "both
    /// error". Covers each routed abort class: `fail(msg)` (dynamic), list-index
    /// OOB and `string.to_int` junk (static + dynamic data), and NaN ordering
    /// (static). This is the differential gate's semantics made a unit test: a
    /// compiled trap at the wrong site or for the wrong reason would diverge here.
    #[test]
    fn abort_messages_match_across_backends() {
        // Each case: (program, expected message core the interpreter produces).
        let cases: &[(&str, &str)] = &[
            (
                "fn main(console: Console):\n    fail(\"the reason\")\n",
                "the reason",
            ),
            (
                "import list\nfn main(console: Console):\n    let xs = [1, 2]\n    console.print(\"${list.at(xs, 5)}\")\n",
                "list index 5 out of bounds (length 2)",
            ),
            (
                "fn main(console: Console):\n    console.print(\"${\"junk\".to_int()}\")\n",
                "cannot parse `junk` as an Int",
            ),
            (
                "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    console.print(\"${nan < 1.0}\")\n",
                "cannot compare NaN",
            ),
        ];
        // (RFC-0045 / latent i32-wrap hole) A list index beyond i32 range must
        // still abort with its TRUE value on both backends — `$list_at` now checks
        // in i64, so a huge index can't wrap to an in-range i32 and read a bogus
        // slot. `4294967297` = 2^32 + 1 (wraps to 1 as i32) is the regression seed.
        let wrap_src = "import list\nfn main(console: Console):\n    let xs = [10, 20]\n    console.print(\"${list.at(xs, 4294967297)}\")\n";
        {
            let ierr = interpreter::run(wrap_src).expect_err("interpreter must abort on the huge index");
            assert!(
                ierr.message.ends_with("list index 4294967297 out of bounds (length 2)"),
                "interpreter: {}",
                ierr.message
            );
            let linked = resolve_std_src(wrap_src);
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("binary");
            let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort on the huge index");
            assert_eq!(
                cerr,
                format!("runtime error: {}", ierr.message),
                "compiled must report the TRUE index, not a wrapped one"
            );
        }

        for (src, want_core) in cases {
            // Interpreter (the oracle): its full message ends with the core.
            let ierr = interpreter::run(src).expect_err("interpreter must abort");
            assert!(
                ierr.message.ends_with(want_core),
                "interpreter core mismatch: got `{}`, want suffix `{want_core}`",
                ierr.message
            );
            // Compiled: the routed abort surfaces `runtime error: <core>` via the
            // host `bail!` (root cause). It must equal the interpreter's core.
            let linked = resolve_std_src(src);
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort");
            assert_eq!(
                cerr,
                format!("runtime error: {}", ierr.message),
                "compiled abort mismatch for src:\n{src}"
            );
        }
    }

    /// (RFC-0044 rule 3) The pure-witchy std contract-violation aborts: a bad
    /// argument that used to silently default now aborts, with the SAME message
    /// on both backends (they run the identical std source; RFC-0045 routes the
    /// message). Each case pairs a program with its message core.
    #[test]
    fn std_contract_violations_abort_on_both_backends() {
        let cases: &[(&str, &str)] = &[
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.factorial(-5)}\")\n",
                "math.factorial: `-5` is negative (expected n >= 0)",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.pow(2, -1)}\")\n",
                "math.pow: exponent `-1` is negative (expected exp >= 0)",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.isqrt(-5)}\")\n",
                "math.isqrt: `-5` is negative (expected n >= 0)",
            ),
            (
                "import time\nfn main(console: Console):\n    console.print(\"${time.days_in_month(2026, 13)}\")\n",
                "time.days_in_month: month `13` is out of range (expected 1..12)",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(math.to_base(10, 17))\n",
                "math.to_base: base `17` is outside 2..16",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(math.to_base(10, 1))\n",
                "math.to_base: base `1` is outside 2..16",
            ),
            (
                "fn main(console: Console):\n    console.print(\"x\".pad_left(3, \"\"))\n",
                "string.pad_left: empty `fill` cannot pad to width 3",
            ),
            (
                "fn main(console: Console):\n    console.print(\"x\".pad_right(3, \"\"))\n",
                "string.pad_right: empty `fill` cannot pad to width 3",
            ),
            (
                "fn main(console: Console):\n    console.print(\"x\".center(3, \"\"))\n",
                "string.center: empty `fill` cannot pad to width 3",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.clamp(5, 10, 0)}\")\n",
                "math.clamp: lo `10` exceeds hi `0`",
            ),
            (
                "import cmp\nfn main(console: Console):\n    console.print(\"${cmp.clamp(5, 10, 0)}\")\n",
                "cmp.clamp: lo exceeds hi (an empty range)",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.ceil_div(7, 0)}\")\n",
                "math.ceil_div: divisor `0` must be positive",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.round_div(7, -2)}\")\n",
                "math.round_div: divisor `-2` must be positive",
            ),
            (
                "import semver\nfn main(console: Console):\n    console.print(semver.format(semver.version(-1, 2, 3)))\n",
                "semver.version: components `-1.2.3` must be non-negative",
            ),
        ];
        for (src, want_core) in cases {
            // The interpreter resolves the std bodies at run time only when they are
            // linked in (these are real fn bodies, not builtins), so link first.
            let linked = resolve_std_src(src);
            let ierr = interpreter::run_module(linked.clone(), ".", Vec::new())
                .expect_err("interpreter must abort");
            assert!(
                ierr.message.ends_with(want_core),
                "interpreter core mismatch: got `{}`, want suffix `{want_core}`",
                ierr.message
            );
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort");
            assert_eq!(
                cerr,
                format!("runtime error: {}", ierr.message),
                "compiled abort mismatch for src:\n{src}"
            );
        }
        // The valid-boundary values still work (no over-eager abort): factorial(0),
        // pow(x, 0), isqrt(0), days_in_month for every month 1..12, to_base at both
        // ends of 2..16, and an empty fill when the string is already wide enough
        // (no padding is needed, so nothing is violated).
        let ok = "import math\nimport time\nfn main(console: Console):\n    console.print(\"${math.factorial(0)}\")\n    console.print(\"${math.pow(2, 0)}\")\n    console.print(\"${math.isqrt(0)}\")\n    console.print(\"${time.days_in_month(2024, 2)}\")\n    console.print(\"${time.days_in_month(2026, 2)}\")\n    console.print(\"${time.days_in_month(2026, 12)}\")\n    console.print(math.to_base(10, 2))\n    console.print(math.to_base(255, 16))\n    console.print(\"abc\".pad_left(3, \"\"))\n    console.print(\"abcd\".center(3, \"\"))\n";
        let want = vec!["1", "1", "0", "29", "28", "31", "1010", "ff", "abc", "abcd"];
        assert_eq!(link_run(ok), want, "interpreter boundary");
        assert_eq!(wasm_run(ok), want, "compiled boundary");
    }

    /// Small stdlib edge contracts, pinned on both backends: `path.base("/")`
    /// honors its documented root case; `list.chunks` yields `[]` for a
    /// non-positive size (there are no chunks of length 0) like `windows`;
    /// `time.format` preserves a trailing bare `%` like any other unknown
    /// directive; `duration.human`/`clock` render a negative span as a signed
    /// magnitude, never truncated-division fields; `ascii` predicates reject
    /// multi-character strings instead of classifying by lexicographic prefix.
    #[test]
    fn stdlib_edge_contracts_backends_agree() {
        let src = r#"import path
import list
import time
import duration
import ascii
import option

fn show_chunks(xs: List(List(Int))) -> String:
    "[" + list.join(list.map(xs, fn(c: List(Int)): "[" + list.join(list.map(c, fn(x: Int): "${x}"), ",") + "]"), ";") + "]"

fn main(console: Console):
    console.print(path.base("/") + "|" + path.stem("/") + "|" + path.base("a/b/"))
    console.print(path.base("") + "|" + (path.dir("/") ?? "<none>") + "|" + (path.ext("..") ?? "<none>") + "|" + path.stem("..") + "|[" + (path.ext("foo.") ?? "<none>") + "]|" + path.stem("foo."))
    console.print(show_chunks(list.chunks([1, 2, 3], 2)) + "|" + show_chunks(list.chunks([1, 2, 3], 0)) + "|" + show_chunks(list.chunks([1, 2, 3], -1)))
    match time.civil(2026, 7, 5, 12, 34, 56):
        Ok(d) -> console.print(time.format(d, "done %") + "|" + time.format(d, "done %%") + "|" + time.format(d, "done %Q"))
        Err(e) -> console.print(time.time_error_message(e))
    console.print(duration.human(duration.seconds(0 - 1)) + "|" + duration.human(duration.minutes(0 - 1)) + "|" + duration.human(duration.milliseconds(0 - 1)) + "|" + duration.human(duration.seconds(90)))
    console.print(duration.clock(duration.seconds(0 - 1)) + "|" + duration.clock(duration.seconds(3661)))
    console.print("${ascii.is_digit("55")}|${ascii.is_digit("5")}|${ascii.is_upper("ABC")}|${ascii.is_upper("A")}|${ascii.is_lower("az")}|${ascii.is_lower("z")}")
    console.print("${ascii.to_digit("55")}|${ascii.to_digit("7")}|${ascii.is_digit("")}")
"#;
        let interpreted = link_run(src);
        let compiled = wasm_run(src);
        assert_eq!(interpreted, compiled, "stdlib edge contracts diverged");
        assert_eq!(
            compiled,
            vec![
                "/|/|b",
                ".|<none>|<none>|..|[]|foo",
                "[[1,2];[3]]|[]|[]",
                "done %|done %|done %Q",
                "-1s|-1m0s|-1ms|1m30s",
                "-0:00:01|1:01:01",
                "false|true|false|true|false|true",
                "None|Some(7)|false",
            ]
        );
    }

    /// Batch-2 stdlib edge contracts, pinned on both backends: the string
    /// module's empty-pattern rule is uniform (an empty pattern matches
    /// NOTHING — `index_of`/`split_once`/`replace_first` now agree with
    /// `count`/`last_index_of`/`rsplit_once`); semver rejects plus-signed
    /// components and still parses/orders normally; base64url (the no-padding
    /// JWT/WebAuthn form) rejects padded input that plain base64 accepts;
    /// oauth.authorize_url extends a query-bearing endpoint with `&`.
    #[test]
    fn stdlib_edge_contracts_batch2_backends_agree() {
        let src = r#"import semver
import encoding
import oauth
import option

fn ok_err(r: Result(String, encoding.EncodingError)) -> String:
    match r:
        Ok(_) -> "ok"
        Err(_) -> "err"

fn ver(s: String) -> String:
    match semver.parse(s):
        Ok(v) -> semver.format(v)
        Err(_) -> "err"

fn main(console: Console):
    let (a1, a2) = "abc".split_once("")
    console.print("abc".replace_first("", "X") + "|" + a1 + "," + a2 + "|" + "${"abc".index_of("")}" + "|" + "${"abc".count("")}")
    let (b1, b2) = "k=v".split_once("=")
    console.print("aXc".replace_first("X", "b") + "|" + b1 + "," + b2)
    console.print(ver("1.2.3") + "|" + ver("+1.2.3") + "|" + ver("1.+2.3") + "|" + ver("-1.2.3"))
    console.print(ok_err(encoding.base64url_decode("SGk")) + "|" + ok_err(encoding.base64url_decode("SGk=")) + "|" + ok_err(encoding.base64_decode("SGk=")))
    console.print(oauth.authorize_url("https://idp/auth?prompt=consent", "c", "https://app/cb", "openid", "s"))
    console.print(oauth.authorize_url("https://idp/auth", "c", "https://app/cb", "openid", "s"))
"#;
        let interpreted = link_run(src);
        let compiled = wasm_run(src);
        assert_eq!(interpreted, compiled, "batch-2 edge contracts diverged");
        assert_eq!(
            compiled,
            vec![
                "abc|abc,|None|0",
                "abc|k,v",
                "1.2.3|err|err|err",
                "ok|err|ok",
                "https://idp/auth?prompt=consent&response_type=code&client_id=c&redirect_uri=https%3A%2F%2Fapp%2Fcb&scope=openid&state=s",
                "https://idp/auth?response_type=code&client_id=c&redirect_uri=https%3A%2F%2Fapp%2Fcb&scope=openid&state=s",
            ]
        );
    }

    /// Batch-3 stdlib edge contracts, pinned on both backends: `list.min`/`max`
    /// are generic over `Ord` like `sort` (Strings, Durations — not just Int);
    /// `url.parse` normalizes the case-insensitive scheme so `HTTPS://` gets
    /// port 443 and formats canonically; `server.with_header` stores lowercase
    /// names so `http.header` lookup works for any spelling; the HTTP client
    /// drops a caller-supplied Host (the renderer owns it, like the framing
    /// headers); `server.render_for` suppresses a HEAD response's body while
    /// keeping its Content-Length; a large `iter.drop` skips iteratively.
    #[test]
    fn stdlib_edge_contracts_batch3_backends_agree() {
        let src = r#"import list
import url
import http
import server
import iter
import option
import duration

fn show_url(raw: String) -> String:
    match url.parse(raw):
        Err(_e) -> "err"
        Ok(u) -> url.scheme(u) + " " + "${url.port(u)}" + " " + url.format(u)

fn main(console: Console):
    console.print((list.min(["pear", "apple", "plum"]) ?? "none") + "|" + (list.max(["pear", "apple", "plum"]) ?? "none"))
    console.print("${list.min([3, 1, 4]) ?? 0}|${list.max([3, 1, 4]) ?? 0}")
    console.print(duration.human(list.max([duration.seconds(5), duration.minutes(1)]) ?? duration.seconds(0)))
    console.print(show_url("HTTPS://example.test/p") + "|" + show_url("https://example.test/p"))
    let resp = server.with_header(server.ok("body"), "X-Trace-Id", "abc")
    console.print((http.header(resp, "x-trace-id") ?? "none") + "|" + (http.header(resp, "X-Trace-Id") ?? "none"))
    let head_wire = server.render_for(server.text(200, "body"), "HEAD")
    let get_wire = server.render_for(server.text(200, "body"), "GET")
    console.print("${head_wire.contains("Content-Length: 4")}|${head_wire.ends_with("\r\n\r\n")}|${get_wire.ends_with("body")}")
    let tail: List(Int) = iter.collect(iter.range(0, 100000).drop(99997))
    console.print("${tail}")
"#;
        let interpreted = link_run(src);
        let compiled = wasm_run(src);
        assert_eq!(interpreted, compiled, "batch-3 edge contracts diverged");
        assert_eq!(
            compiled,
            vec![
                "apple|plum",
                "1|4",
                "1m0s",
                "https 443 https://example.test/p|https 443 https://example.test/p",
                "abc|abc",
                "true|true|true",
                "[99997, 99998, 99999]",
            ]
        );
    }

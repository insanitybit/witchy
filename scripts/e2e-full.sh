#!/usr/bin/env bash
# The from-scratch compiler acceptance test: build witchy and exercise the
# compiler toolchain end to end with asserted outputs — backend parity,
# capability auditing and sandbox enforcement, the formatter, the in-language
# test framework, and doc extraction.
#
#   ./scripts/e2e-full.sh           # everything, including `cargo test`
#   ./scripts/e2e-full.sh --quick   # skip the Rust test suites (the gate runs them
#                                   # in their own job)
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$PWD"
QUICK=0
[ "${1:-}" = "--quick" ] && QUICK=1

PASS=0
stage() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
ok() { PASS=$((PASS + 1)); echo "  ok: $*"; }
die() { printf '\033[31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }
expect_eq() { # description, expected, actual
    [ "$2" = "$3" ] || die "$1
  want: $2
   got: $3"
    ok "$1"
}
expect_contains() { # description, needle, haystack
    case "$3" in *"$2"*) ok "$1" ;; *) die "$1 — missing \`$2\` in:
$3" ;; esac
}
expect_fails() { # description, command...
    local desc="$1"; shift
    if "$@" >/dev/null 2>&1; then die "$desc — succeeded but must fail"; fi
    ok "$desc"
}

WORK="$(mktemp -d "${TMPDIR:-/tmp}/witchy-e2e.XXXXXX")"
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

stage "0. Build from scratch"
cargo build --release --quiet
# Honor a custom CARGO_TARGET_DIR (concurrent agents build into per-agent target
# dirs). A relative override resolves under $REPO (cargo ran from here); an
# absolute one is used as-is.
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$REPO/$TARGET_DIR" ;; esac
BIN="$TARGET_DIR/release/witchy"
ok "release build"

if [ "$QUICK" = 0 ]; then
    stage "1. The Rust test suites"
    cargo test --release --quiet >/dev/null
    ok "cargo test"
else
    stage "1. (skipped: --quick) Rust test suites"
fi

stage "2. One program, three backends, identical output"
cat > "$WORK/lang.witchy" <<'EOF'
import option

type Shape:
    Circle(Int)
    Square(Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> 3 * r * r
        Square(w) -> w * w

fn main(console: Console):
    let shapes = [Circle(2), Square(3)]
    var total = 0
    for s in shapes:
        total = total + area(s)
    console.print("total: ${total}")
    let add = fn(n: Int): n + total
    console.print("${add(1)}")
    console.print("${[1, 2] == [1, 2]}")
    console.print("${Some("a") == Some("a")}")
    var d: Dict(String, Int) = dict.new()
    d.insert("k", 1)
    d.insert("j", 2)
    console.print("${dict.get_or(d, "j", 0)}")
    console.print("${1500ms < 2s}")
EOF
WANT="$(printf 'total: 21\n22\ntrue\ntrue\n2\ntrue')"
GOT_INTERP="$("$BIN" "$WORK/lang.witchy")"
expect_eq "interpreter output" "$WANT" "$GOT_INTERP"
# Classify parity by its EXIT CODE + the machine-readable `outcome=` token, not by a
# substring of the human line (RFC-0058 §2): "agree" is a substring of "both error" /
# "disagree" too, and a compile regression would still print it.
set +e
PARITY_OUT="$("$BIN" parity "$WORK/lang.witchy" 2>&1)"
PARITY_CODE=$?
set -e
expect_eq "interpreter <-> WASM parity exits 0 (agree)" "0" "$PARITY_CODE"
expect_contains "parity reports machine outcome=agree" "outcome=agree" "$PARITY_OUT"
expect_contains "type-checks standalone" "" "$("$BIN" check "$WORK/lang.witchy" 2>&1)"
test -n "$("$BIN" emit-wat "$WORK/lang.witchy")" && ok "emit-wat produces a module"

stage "3. The formatter: canonical and idempotent"
printf 'fn main(console: Console):\n        console.print("x")\n' > "$WORK/ugly.witchy"
"$BIN" fmt "$WORK/ugly.witchy" >/dev/null
"$BIN" fmt --check "$WORK/ugly.witchy" >/dev/null && ok "fmt then fmt --check passes"
ONCE="$(cat "$WORK/ugly.witchy")"
"$BIN" fmt "$WORK/ugly.witchy" >/dev/null
expect_eq "fmt is idempotent" "$ONCE" "$(cat "$WORK/ugly.witchy")"

stage "4. The in-language test framework"
cat > "$WORK/suite.witchy" <<'EOF'
import testing

fn double(n: Int) -> Int:
    n * 2

fn test_double():
    testing.assert_int_eq(double(21), 42)

fn test_strings():
    testing.assert_eq("a" + "b", "ab")
EOF
expect_contains "passing suite reports ok" "2 passed; 0 failed" "$("$BIN" test "$WORK/suite.witchy")"
cat >> "$WORK/suite.witchy" <<'EOF'

fn test_broken():
    testing.assert(1 > 2, "deliberately wrong")
EOF
expect_fails "failing suite exits non-zero" "$BIN" test "$WORK/suite.witchy"
expect_contains "failure carries the message" "deliberately wrong" "$("$BIN" test "$WORK/suite.witchy" || true)"

stage "5. Capability auditing: footprints are computed, widening is loud"
cat > "$WORK/v1.witchy" <<'EOF'
pub fn load(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn main(console: Console, dir: Dir[Read]):
    console.print(load(dir, "x"))
EOF
expect_contains "caps reports the rights-split footprint" "Dir[Read]" "$("$BIN" caps "$WORK/v1.witchy")"
cat > "$WORK/v2.witchy" <<'EOF'
pub fn load(dir: Dir, name: String) -> String:
    dir.write("audit.log", name)
    dir.read(name)

fn main(console: Console, dir: Dir):
    console.print(load(dir, "x"))
EOF
set +e
"$BIN" caps-diff "$WORK/v1.witchy" "$WORK/v2.witchy" >/dev/null 2>&1
CODE=$?
set -e
expect_eq "caps-diff exits 2 on widening (Read -> Read+Write)" "2" "$CODE"

stage "6. Sandbox enforcement: confined filesystem + env + argv in the VM"
mkdir -p "$WORK/jail/sub"
printf 'find the needle here\nnothing\n' > "$WORK/jail/data.txt"
cat > "$WORK/grep.witchy" <<'EOF'
import option
import string

fn main(console: Console, env: Env, dir: Dir[Read], args: List(String)) -> Int:
    let label = match env.get_env("E2E_LABEL"):
        Some(v) -> v
        None -> "unset"
    for line in string.lines(dir.read(list.at(args, 0))):
        if string.contains(line, "needle"):
            console.print(label + ": " + line)
    0
EOF
GOT="$(E2E_LABEL=found "$BIN" sandbox --dir "$WORK/jail" "$WORK/grep.witchy" data.txt 2>/dev/null)"
expect_eq "sandboxed Dir+Env+argv program" "found: find the needle here" "$GOT"
printf 'secret\n' > "$WORK/outside.txt"
cat > "$WORK/escape.witchy" <<'EOF'
fn main(console: Console, dir: Dir[Read]):
    console.print(dir.read("../outside.txt"))
EOF
expect_fails "a ../ escape is refused by the VM" "$BIN" sandbox --dir "$WORK/jail" "$WORK/escape.witchy"
cat > "$WORK/dialer.witchy" <<'EOF'
fn main(console: Console, net: Net[Connect]):
    let sock = net.connect("203.0.113.1:80")
    console.print("connected")
EOF
expect_fails "an address outside the --net allowlist is refused" "$BIN" sandbox "$WORK/dialer.witchy"

stage "7. Documentation extraction"
expect_contains "witchy doc renders the std API" "##" "$("$BIN" doc "$REPO/std/list.witchy")"

printf '\n\033[1m== ALL STAGES PASSED (%d checks) ==\033[0m\n' "$PASS"

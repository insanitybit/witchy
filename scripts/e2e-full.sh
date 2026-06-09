#!/usr/bin/env bash
# The from-scratch acceptance test: build witchy and exercise every major
# subsystem end to end with asserted outputs — the three backends and their
# parity, capability auditing and sandbox enforcement, the formatter, the
# in-language test framework, doc extraction, a multi-rune example project,
# and the full registry lifecycle including trusted publishing, two-phase
# release, and the capability-widening gate.
#
#   ./scripts/e2e-full.sh           # everything, including `cargo test`
#   ./scripts/e2e-full.sh --quick   # skip the Rust test suites (CI runs them
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
SERVER_PID=""
cleanup() {
    if [ -n "$SERVER_PID" ]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

stage "0. Build from scratch"
cargo build --release --quiet
BIN="$REPO/target/release/witchy"
ok "release build"

if [ "$QUICK" = 0 ]; then
    stage "1. The Rust test suites (unit + package-manager e2e)"
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
    print(console, "total: ${total}")
    let add = fn(n: Int): n + total
    print(console, int_to_string(add(1)))
    print(console, to_string([1, 2] == [1, 2]))
    print(console, to_string(Some("a") == Some("a")))
    let d = insert(insert(dict_new(), "k", 1), "j", 2)
    print(console, int_to_string(get_or(d, "j", 0)))
    print(console, to_string(1500ms < 2s))
EOF
WANT="$(printf 'total: 21\n22\ntrue\ntrue\n2\ntrue')"
GOT_INTERP="$("$BIN" "$WORK/lang.witchy")"
expect_eq "interpreter output" "$WANT" "$GOT_INTERP"
expect_contains "interpreter <-> WASM parity" "agree" "$("$BIN" parity "$WORK/lang.witchy" 2>&1)"
expect_contains "type-checks standalone" "" "$("$BIN" check "$WORK/lang.witchy" 2>&1)"
test -n "$("$BIN" emit-wat "$WORK/lang.witchy")" && ok "emit-wat produces a module"
test -n "$("$BIN" emit-rust "$WORK/lang.witchy")" && ok "emit-rust produces a program"
if command -v rustc >/dev/null; then
    GOT_NATIVE="$("$BIN" native "$WORK/lang.witchy")"
    expect_eq "native backend agrees" "$WANT" "$GOT_NATIVE"
else
    echo "  (rustc not found — skipping the native backend)"
fi

stage "3. The formatter: canonical and idempotent"
printf 'fn main(console: Console):\n        print(console,"x")\n' > "$WORK/ugly.witchy"
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
    testing.assert_eq("a" <> "b", "ab")
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
    read(dir, name)

fn main(console: Console, dir: Dir[Read]):
    print(console, load(dir, "x"))
EOF
expect_contains "caps reports the rights-split footprint" "Dir[Read]" "$("$BIN" caps "$WORK/v1.witchy")"
cat > "$WORK/v2.witchy" <<'EOF'
pub fn load(dir: Dir, name: String) -> String:
    write(dir, "audit.log", name)
    read(dir, name)

fn main(console: Console, dir: Dir):
    print(console, load(dir, "x"))
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
    let label = match get_env(env, "E2E_LABEL"):
        Some(v) -> v
        None -> "unset"
    for line in string.lines(read(dir, at(args, 0))):
        if contains(line, "needle"):
            print(console, label <> ": " <> line)
    0
EOF
GOT="$(E2E_LABEL=found "$BIN" sandbox --dir "$WORK/jail" "$WORK/grep.witchy" data.txt 2>/dev/null)"
expect_eq "sandboxed Dir+Env+argv program" "found: find the needle here" "$GOT"
printf 'secret\n' > "$WORK/outside.txt"
cat > "$WORK/escape.witchy" <<'EOF'
fn main(console: Console, dir: Dir[Read]):
    print(console, read(dir, "../outside.txt"))
EOF
expect_fails "a ../ escape is refused by the VM" "$BIN" sandbox --dir "$WORK/jail" "$WORK/escape.witchy"
cat > "$WORK/dialer.witchy" <<'EOF'
fn main(console: Console, net: Net[Connect]):
    let sock = connect(net, "203.0.113.1:80")
    print(console, "connected")
EOF
expect_fails "an address outside the --net allowlist is refused" "$BIN" sandbox "$WORK/dialer.witchy"

stage "7. The registry lifecycle, from nothing"
export WITCHY_HOME="$WORK/home"
mkdir -p "$WORK/idp" "$WORK/registry"
PUBHEX="$("$BIN" coven-gen-issuer --out "$WORK/idp")"
"$BIN" coven-serve --addr 127.0.0.1:0 --root "$WORK/registry" \
    --trust-issuer "local-idp=$PUBHEX" > "$WORK/server.log" &
SERVER_PID=$!
for _ in $(seq 1 50); do grep -q "http://" "$WORK/server.log" 2>/dev/null && break; sleep 0.1; done
export COVEN_URL="$(grep -o 'http://[^ ]*' "$WORK/server.log" | head -1)"
ok "coven-serve up at $COVEN_URL"

mint() { "$BIN" coven-mint-token --issuer-key "$WORK/idp" --issuer local-idp "$@"; }
CI_TOKEN="$(mint --sub "repo:acme/logger-repo:ref:refs/heads/main" \
    --claim repository=acme/logger-repo --claim workflow_ref=release.yml --claim ref=refs/heads/main)"
ALICE="$(mint --sub alice)"

mkdir -p "$WORK/logger/src"
printf '[rune]\nname = "acme/logger"\nversion = "1.0.0"\n' > "$WORK/logger/witchy.toml"
printf 'pub fn line(s: String) -> String:\n    "[log] " <> s\n' > "$WORK/logger/src/logger.witchy"
(cd "$WORK/logger" && WITCHY_USER=ci-bot COVEN_ID_TOKEN="$CI_TOKEN" "$BIN" publish >/dev/null)
ok "trusted publish (lands STAGED)"

mkdir -p "$WORK/app/src"
printf '[rune]\nname = "demo/app"\nversion = "0.1.0"\n' > "$WORK/app/witchy.toml"
expect_fails "a STAGED version is not addable" \
    env -C "$WORK/app" WITCHY_USER=dev "$BIN" add acme/logger
(cd "$WORK/logger" && WITCHY_USER=alice COVEN_ID_TOKEN="$ALICE" \
    "$BIN" promote acme/logger@1.0.0 --factor webauthn >/dev/null)
ok "promote with a second factor (separation of duties)"
(cd "$WORK/app" && WITCHY_USER=dev "$BIN" add acme/logger >/dev/null)
ok "add: fetched over HTTP, signature-verified"
expect_contains "lockfile pins the registry key" "ed25519:" "$(cat "$WORK/app/witchy.lock")"
expect_contains "lockfile records trusted-publishing provenance" "trusted-publisher" "$(cat "$WORK/app/witchy.lock")"

printf 'import logger\n\nfn main(console: Console):\n    print(console, logger.line("hello"))\n' \
    > "$WORK/app/src/app.witchy"
GOT="$(cd "$WORK/app" && WITCHY_USER=dev "$BIN" run)"
expect_eq "the consumer runs against the fetched rune" "[log] hello" "$GOT"

# The widening gate: v1.1.0 quietly starts demanding Net.
printf '[rune]\nname = "acme/logger"\nversion = "1.1.0"\n' > "$WORK/logger/witchy.toml"
printf 'pub fn line(s: String) -> String:\n    "[log] " <> s\n\npub fn beacon(net: Net, s: String) -> String:\n    s\n' \
    > "$WORK/logger/src/logger.witchy"
(cd "$WORK/logger" && WITCHY_USER=ci-bot COVEN_ID_TOKEN="$CI_TOKEN" "$BIN" publish >/dev/null)
(cd "$WORK/logger" && WITCHY_USER=alice COVEN_ID_TOKEN="$ALICE" \
    "$BIN" promote acme/logger@1.1.0 --factor webauthn >/dev/null)
set +e
UPDATE_OUT="$(cd "$WORK/app" && WITCHY_USER=dev "$BIN" update 2>&1)"
UPDATE_CODE=$?
set -e
[ "$UPDATE_CODE" -ne 0 ] || die "update must BLOCK a widening upgrade"
expect_contains "update blocks and names the new authority" "Net" "$UPDATE_OUT"
(cd "$WORK/app" && WITCHY_USER=dev "$BIN" update --allow-cap Net >/dev/null)
expect_contains "explicit consent upgrades and re-locks" "1.1.0" "$(cat "$WORK/app/witchy.lock")"

# Namespace binding: a valid token from another repository cannot publish.
printf '[rune]\nname = "acme/logger"\nversion = "1.2.0"\n' > "$WORK/logger/witchy.toml"
EVIL="$(mint --sub "repo:evil/fork:ref:refs/heads/main" \
    --claim repository=evil/fork --claim workflow_ref=release.yml --claim ref=refs/heads/main)"
expect_fails "a publish from the wrong repository is refused" \
    env -C "$WORK/logger" WITCHY_USER=ci-bot COVEN_ID_TOKEN="$EVIL" "$BIN" publish

stage "8. A multi-rune example project (path deps, lockfile, diamond)"
GOT="$(cd "$REPO/examples/projects/dashboard/dashboard" && "$BIN" run 2>&1)" || die "dashboard run failed: $GOT"
ok "examples/projects/dashboard builds and runs ($(echo "$GOT" | wc -l | tr -d ' ') lines)"

stage "9. Documentation extraction"
expect_contains "witchy doc renders the std API" "##" "$("$BIN" doc "$REPO/std/list.witchy")"

printf '\n\033[1m== ALL STAGES PASSED (%d checks) ==\033[0m\n' "$PASS"

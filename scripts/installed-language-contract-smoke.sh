#!/usr/bin/env bash
# Prove the installed compiler accepts the RFC-0167 core surface and rejects
# the retired spellings, without reading compiler inputs from this checkout.
set -euo pipefail

binary=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --witchy) [ "$#" -ge 2 ] || exit 2; binary="$2"; shift 2 ;;
        -h|--help) echo "usage: installed-language-contract-smoke.sh --witchy <binary>"; exit 0 ;;
        *) echo "installed-language-contract-smoke: unknown argument '$1'" >&2; exit 2 ;;
    esac
done
[ -f "$binary" ] && [ -x "$binary" ] && [ ! -L "$binary" ] || {
    echo "installed-language-contract-smoke: binary is missing, non-executable, or a symlink: $binary" >&2
    exit 1
}

scratch="$(mktemp -d "${TMPDIR:-/tmp}/witchy-installed-language.XXXXXX")"
cleanup() { rm -rf "$scratch"; }
trap cleanup EXIT HUP INT TERM
install="$scratch/install"
work="$scratch/work"
mkdir -p "$install/bin" "$work"
cp "$binary" "$install/bin/witchy"

run_witchy() {
    env -i HOME="$scratch/home" TMPDIR="$scratch/tmp" XDG_CACHE_HOME="$scratch/cache" \
        PATH="$install/bin" "$install/bin/witchy" "$@"
}
mkdir -p "$scratch/home" "$scratch/tmp" "$scratch/cache"

cat > "$work/core.witchy" <<'EOF'
fn consume(own value: String) -> String:
    value

fn round_trip(own value: unique Int) -> unique Int:
    value

fn frozen_value(value: frozen String) -> frozen String:
    value

fn main(console: Console):
    let empty: () = ()
    let value: unique Int = 7
    let result = round_trip(value)
    let _ = consume("owned")
    let _ = frozen_value("frozen")
    let _ = result
    console.print("rfc0167-core")
EOF
run_witchy check "$work/core.witchy" >/dev/null
run_witchy fmt --check "$work/core.witchy" >/dev/null
run_witchy compile "$work/core.witchy" --out "$work/core.wasm" >/dev/null
[ -s "$work/core.wasm" ] || {
    echo "installed-language-contract-smoke: standalone compile produced no artifact" >&2
    exit 1
}
[ "$(run_witchy "$work/core.witchy")" = "rfc0167-core" ] || {
    echo "installed-language-contract-smoke: canonical core program output mismatch" >&2
    exit 1
}
parity_output="$(run_witchy parity "$work/core.witchy")"
case "$parity_output" in
    *"interpreter and compiled WASM agree"*) ;;
    *)
        echo "installed-language-contract-smoke: public interpreter/Wasm parity did not report agreement: $parity_output" >&2
        exit 1
        ;;
esac

check_rejected() {
    local name="$1"; shift
    local expected="$1"; shift
    local source="$1"
    printf '%b\n' "$source" > "$work/$name.witchy"
    local output
    if output="$(run_witchy check "$work/$name.witchy" 2>&1)"; then
        echo "installed-language-contract-smoke: check accepted retired spelling: $name" >&2
        exit 1
    fi
    case "$output" in
        *"$expected"*) ;;
        *)
            echo "installed-language-contract-smoke: wrong diagnostic for retired spelling $name: $output" >&2
            exit 1
            ;;
    esac
}
check_rejected view "View(T, 'a)" "fn bad(value: View(String, 'a)):\n    value"
check_rejected let_lifetime "let('a) T" "fn bad(value: let('a) String) -> String:\n    value"
check_rejected var_lifetime "var('a)" "fn bad(var('a) value: String) -> String:\n    value"
check_rejected local_unique "local unique" "fn bad(value: local unique String) -> String:\n    value"
check_rejected nil "Nil" "fn bad() -> Nil:\n    ()"

echo "installed-language-contract-smoke: PASS"

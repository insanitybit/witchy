#!/usr/bin/env bash
# Prove RFC-0129 row 7 with a public binary copied into a clean install root.
# shellcheck disable=SC2016 # Witchy interpolation must remain literal.
set -euo pipefail

binary=""

usage() {
    echo "usage: installed-bounded-channels-smoke.sh --witchy <binary>" >&2
}
while [ "$#" -gt 0 ]; do
    case "$1" in
        --witchy)
            [ "$#" -ge 2 ] || { usage; exit 2; }
            binary="$2"
            shift 2
            ;;
        -h | --help) usage; exit 0 ;;
        *) echo "installed-bounded-channels-smoke: unknown argument '$1'" >&2; usage; exit 2 ;;
    esac
done
[ -n "$binary" ] || { usage; exit 2; }
[ -f "$binary" ] && [ -x "$binary" ] && [ ! -L "$binary" ] || {
    echo "installed-bounded-channels-smoke: binary is missing, non-executable, or a symlink: $binary" >&2
    exit 1
}

CP="$(command -v cp)"
ENV="$(command -v env)"
MKDIR="$(command -v mkdir)"
MKTEMP="$(command -v mktemp)"
RM="$(command -v rm)"
SH="$(command -v sh)"

scratch="$($MKTEMP -d "${TMPDIR:-/tmp}/witchy-installed-bounded.XXXXXX")"
cleanup() { "$RM" -rf "$scratch"; }
trap cleanup EXIT HUP INT TERM

install="$scratch/install"
work="$scratch/work"
home="$scratch/home"
tmp="$scratch/tmp"
cache="$scratch/cache"
"$MKDIR" -p "$install/bin" "$work/bounded-channels/src" "$home" "$tmp" "$cache"
"$CP" "$binary" "$install/bin/witchy"

run_witchy() {
    "$ENV" -i \
        HOME="$home" \
        TMPDIR="$tmp" \
        XDG_CACHE_HOME="$cache" \
        PATH="$install/bin" \
        "$install/bin/witchy" "$@"
}

resolved="$("$ENV" -i PATH="$install/bin" "$SH" -c 'command -v witchy')"
[ "$resolved" = "$install/bin/witchy" ] || {
    echo "installed-bounded-channels-smoke: clean install is not the selected PATH binary" >&2
    exit 1
}

cd "$work"
printf '%s\n' \
    '[rune]' \
    'name = "bounded_channels"' \
    'version = "0.1.0"' \
    '' \
    '[dependencies]' > bounded-channels/witchy.toml
printf '%s\n' \
    'from chan import Receiver, Sender' \
    '' \
    'async fn producer(tx: Sender(Int)):' \
    '    for n in [1, 2, 3, 4]:' \
    '        chan.send(tx, n).await' \
    '' \
    'async fn consumer(console: Console, rx: Receiver(Int)):' \
    '    chan.consume(rx, fn(v): chan.done(console.print("got ${v}"))).await' \
    '' \
    'async fn main(console: Console):' \
    '    let (tx, rx) = chan.channel(1).await' \
    '    let producer_handle = chan.spawn(producer(tx)).await' \
    '    consumer(console, rx).await' \
    '    chan.join(producer_handle).await' \
    '    console.print("channel drained")' > bounded-channels/src/bounded_channels.witchy

run_witchy check bounded-channels/src/bounded_channels.witchy >/dev/null
run_witchy build bounded-channels >/dev/null
output="$(run_witchy run bounded-channels)"
expected="got 1
got 2
got 3
got 4
channel drained"
reports="${output#"$expected"}"
if [ "$output" = "$reports" ]; then
    echo "installed-bounded-channels-smoke: bounded workflow output mismatch" >&2
    printf '%s\n' "$output" >&2
    exit 1
fi
reports="${reports#$'\n'}"
if [ -n "$reports" ]; then
    while IFS= read -r report; do
        case "$report" in
            "confinement: layer="*) ;;
            *)
                echo "installed-bounded-channels-smoke: unexpected trailing output" >&2
                printf '%s\n' "$output" >&2
                exit 1
                ;;
        esac
    done <<<"$reports"
fi

echo "installed-bounded-channels-smoke: PASS"

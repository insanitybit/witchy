#!/usr/bin/env bash
# The slice shards, in sequence, in the background; poll status/shards.status.
source "$(dirname "$0")/common.sh"
rm -f "$STATUS/shards.status"
cat > "$STATUS/shards-run.sh" <<INNER
cd "$ROOT"
: > "$STATUS/shards.log"
run() { echo "### \$1" >> "$STATUS/shards.log"; shift; timeout 2400 "\$@" >> "$STATUS/shards.log" 2>&1; echo "exit=\$?" >> "$STATUS/shards.log"; }
run launch cargo test --test misc rfc0165_
run abi cargo test --test misc wasm_abi_catalog
run lower cargo test -p witchy-lower --lib canonical
run wir cargo test -p witchy-wir --lib --features native
run types cargo test -p witchy-types --lib
run runtime cargo test -p witchy-runtime --features native
echo done > "$STATUS/shards.status"
INNER
nohup bash "$STATUS/shards-run.sh" >/dev/null 2>&1 &
disown
echo "shards running; poll $STATUS/shards.status, then grep -E '^###|test result|exit=' $STATUS/shards.log"

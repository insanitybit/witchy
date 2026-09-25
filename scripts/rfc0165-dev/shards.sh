#!/usr/bin/env bash
# RFC-0165 edit probes or broader checkpoints, in the background.
source "$(dirname "$0")/common.sh"
mode="${1:-edit}"
case "$mode" in
    edit | corpus | landing) ;;
    *) echo "usage: $0 [edit|corpus|landing]" >&2; exit 2 ;;
esac
rm -f "$STATUS/shards.status"
cat > "$STATUS/shards-run.sh" <<INNER
cd "$ROOT"
: > "$STATUS/shards.log"
run() { echo "### \$1" >> "$STATUS/shards.log"; shift; timeout 2400 "\$@" >> "$STATUS/shards.log" 2>&1; echo "exit=\$?" >> "$STATUS/shards.log"; }
case "$mode" in
edit)
    # These four probes have the same package, feature, and target selection.
    # Keep the lowerer and root-driver probes separate: their production feature
    # graphs differ, so combining them can hide an activation mistake.
    run wir-prelude-and-prefix cargo nextest run -p witchy-wir --features native --lib -E 'test(constant_prefix_publication_rejects_gaps_and_missing_children)'
    run lower-publication cargo nextest run -p witchy-lower --features witchy-wir/native --test codegen -E 'test(canonical_pipeline::list_string_)'
    run admitted-abi cargo nextest run -p witchy --features witchy-wir/native --test misc --ignore-default-filter -E 'test(architecture::root_integration_test_binaries_stay_within_budget) or test(wasm_abi_catalog::public_spec_contains_the_generated_complete_catalog) or test(rfc0165_admitted_launch::admitted_aggregates_render_structurally_as_the_interpreter_prints_them)'
    ;;
corpus)
    run launch cargo test --test misc rfc0165_
    ;;
landing)
    run launch cargo test --test misc rfc0165_
    run abi cargo test --test misc wasm_abi_catalog
    run lower cargo test -p witchy-lower --test codegen canonical
    run wir cargo test -p witchy-wir --lib --features native
    run types cargo test -p witchy-types --lib
    run runtime cargo test -p witchy-runtime --features native
    ;;
esac
echo done > "$STATUS/shards.status"
INNER
nohup bash "$STATUS/shards-run.sh" >/dev/null 2>&1 &
disown
echo "$mode checks running; poll $STATUS/shards.status, then grep -E '^###|test result|exit=' $STATUS/shards.log"

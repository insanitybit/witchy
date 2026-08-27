#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
MATRIX="$ROOT/rfcs/0162-validator-mutations.schema1.json"
CATALOG_SOURCE="$ROOT/crates/witchy-wir/src/safewir.rs"
TARGET_ROOT=${CARGO_TARGET_DIR:-"$ROOT/target"}
OUTPUT_ROOT="$TARGET_ROOT/rfc0162-validator-mutations"

die() {
    printf 'rfc0162-validator-mutations: %s\n' "$*" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

catalog_rows() {
    awk '
        /^define_rule_ids! \{/ { in_catalog = 1; next }
        in_catalog && /^}/ { exit }
        in_catalog && /=>/ {
            name = $1
            ordinal = $3
            code = $5
            gsub(/[,"]/, "", code)
            print ordinal "\t" name "\t" code
        }
    ' "$CATALOG_SOURCE"
}

matrix_rows() {
    jq -r '.rules | sort_by(.ordinal)[] | [.ordinal, .rule_id, .code] | @tsv' "$MATRIX"
}

validate_matrix_shape() {
    jq -e '
        . as $root
        | ($root.authenticated_sources | map(.path)) as $sources
        | .matrix_schema == 1
        and .safewir_schema == 1
        and (.authenticated_sources | length) == 2
        and ([.authenticated_sources[].path] | unique | length) == 2
        and all(.authenticated_sources[];
            (.path | length > 10) and (.sha256 | test("^[0-9a-f]{64}$")))
        and ($sources | index($root.default_source_file)) != null
        and (.rule_catalog_sha256 | test("^[0-9a-f]{64}$"))
        and (.rules | length) == 20
        and ([.rules[].ordinal] | unique | length) == 20
        and ([.rules[].rule_id] | unique | length) == 20
        and ([.rules[].code] | unique | length) == 20
        and ([.rules[] | select(.evidence_scope == "report-mode-rule")] | length) == 15
        and ([.rules[] | select(.evidence_scope == "deferred-production-authority")] | length) == 5
        and all(.rules[];
            (.ordinal >= 0 and .ordinal < 20)
            and ((.source_file // $root.default_source_file) as $source
                | ($sources | index($source)) != null)
            and (.semantic_weakening | length > 20)
            and (.mutant_name | length > 20)
            and (.selector | length >= 7)
            and (.cargo_test_args | length >= 4)
            and (.focused_test_command == ("./scripts/rfc0162-validator-mutations.sh run " + .code))
            and ((.evidence_scope == "report-mode-rule" and .acceptance_credit == true)
                or (.evidence_scope == "deferred-production-authority" and .acceptance_credit == false))
            and (.observed_outcome == "pending" or .observed_outcome == "CaughtMutant")
        )
        and .summary.rule_count == 20
        and .summary.report_mode_rules == 15
        and .summary.deferred_production_rules == 5
        and .summary.observed_caught == ([.rules[] | select(.observed_outcome == "CaughtMutant")] | length)
        and .summary.observed_survived == 0
        and .summary.observed_timeout == 0
        and .summary.observed_unviable == 0
        and .summary.acceptance_credited_kills == ([.rules[] | select(.acceptance_credit and .observed_outcome == "CaughtMutant")] | length)
        and .summary.acceptance_complete == false
    ' "$MATRIX" >/dev/null || die "matrix shape, partition, commands, or summary is inconsistent"

    if ! diff -u <(catalog_rows) <(matrix_rows); then
        die "matrix does not exactly cover the schema-1 RuleId catalog"
    fi
}

list_mutants() {
    local output=$1
    local source
    local -a file_args=()
    while IFS= read -r source; do
        file_args+=(--file "$source")
    done < <(jq -r '.authenticated_sources[].path' "$MATRIX")
    (
        cd "$ROOT"
        CARGO_BUILD_JOBS=1 cargo mutants \
            --no-config \
            --list \
            --json \
            --package witchy-wir \
            "${file_args[@]}"
    ) >"$output"
}

validate_mutant_selectors() {
    local mutants_json=$1
    local code selector expected count actual
    while IFS=$'\t' read -r code selector expected; do
        count=$(jq --arg selector "$selector" '[.[] | select(.name | test($selector))] | length' "$mutants_json")
        [[ "$count" == 1 ]] || die "$code selector resolved to $count mutants: $selector"
        actual=$(jq -r --arg selector "$selector" '.[] | select(.name | test($selector)) | .name' "$mutants_json")
        [[ "$actual" == "$expected" ]] || die "$code mutant was renamed: expected '$expected', found '$actual'"
    done < <(jq -r '.rules[] | [.code, .selector, .mutant_name] | @tsv' "$MATRIX")
}

check_matrix() {
    need cargo
    need jq
    need awk
    need shasum
    mkdir -p "$OUTPUT_ROOT"
    validate_matrix_shape

    local expected_version actual_version source expected_source actual_source
    local expected_catalog actual_catalog mutants_json
    expected_version=$(jq -r '.cargo_mutants_version' "$MATRIX")
    actual_version=$(cargo mutants --version | awk '{print $2}')
    [[ "$actual_version" == "$expected_version" ]] || \
        die "cargo-mutants version drift: expected $expected_version, found $actual_version"

    while IFS=$'\t' read -r source expected_source; do
        actual_source=$(shasum -a 256 "$ROOT/$source" | awk '{print $1}')
        [[ "$actual_source" == "$expected_source" ]] || \
            die "$source drift: expected $expected_source, found $actual_source"
    done < <(jq -r '.authenticated_sources[] | [.path, .sha256] | @tsv' "$MATRIX")
    expected_catalog=$(jq -r '.rule_catalog_sha256' "$MATRIX")
    actual_catalog=$(shasum -a 256 "$CATALOG_SOURCE" | awk '{print $1}')
    [[ "$actual_catalog" == "$expected_catalog" ]] || \
        die "RuleId catalog drift: expected $expected_catalog, found $actual_catalog"

    mutants_json=$(mktemp "$OUTPUT_ROOT/catalog.XXXXXX")
    list_mutants "$mutants_json"
    validate_mutant_selectors "$mutants_json"
    printf 'matrix ok: 20 RuleIds, 20 unique semantic mutants, cargo-mutants %s\n' "$actual_version"
}

run_rule() {
    local code=$1
    local row_count selector expected_mutant source run_dir before after status outcomes
    local use_taskpolicy=false
    local -a cargo_test_args command

    row_count=$(jq --arg code "$code" '[.rules[] | select(.code == $code)] | length' "$MATRIX")
    [[ "$row_count" == 1 ]] || die "expected one matrix row for $code, found $row_count"
    selector=$(jq -r --arg code "$code" '.rules[] | select(.code == $code) | .selector' "$MATRIX")
    expected_mutant=$(jq -r --arg code "$code" '.rules[] | select(.code == $code) | .mutant_name' "$MATRIX")
    source=$(jq -r --arg code "$code" '
        . as $root | .rules[] | select(.code == $code) | .source_file // $root.default_source_file
    ' "$MATRIX")
    while IFS= read -r argument; do
        cargo_test_args+=("$argument")
    done < <(jq -r --arg code "$code" '.rules[] | select(.code == $code) | .cargo_test_args[]' "$MATRIX")

    run_dir=$(mktemp -d "$OUTPUT_ROOT/${code}.XXXXXX")
    before=$(shasum -a 256 "$ROOT/$source" | awk '{print $1}')
    if command -v taskpolicy >/dev/null 2>&1 \
        && [[ $("$ROOT/scripts/merge-queue.sh" status 2>/dev/null | jq -r '.gate_lock != null') == true ]]; then
        use_taskpolicy=true
    fi
    command=(
        env -u RUSTC_WRAPPER
        CARGO_BUILD_RUSTC_WRAPPER=
        CARGO_BUILD_JOBS=1
        cargo mutants
        --no-config
        --in-place
        --jobserver-tasks 1
        --baseline run
        --test-tool cargo
        --cap-lints true
        --no-shuffle
        --colors never
        --annotations none
        --caught
        --timeout 120
        --build-timeout 600
        --minimum-test-timeout 20
        --output "$run_dir"
        --package witchy-wir
        --file "$source"
        --re "$selector"
        --
        "${cargo_test_args[@]}"
    )

    printf 'running %s: %s\n' "$code" "$expected_mutant"
    set +e
    (
        cd "$ROOT"
        if [[ "$use_taskpolicy" == true ]]; then
            taskpolicy -c utility "${command[@]}"
        else
            "${command[@]}"
        fi
    ) </dev/null 2>&1 | tee "$run_dir/console.log"
    status=${PIPESTATUS[0]}
    set -e

    after=$(shasum -a 256 "$ROOT/$source" | awk '{print $1}')
    [[ "$after" == "$before" ]] || die "$code left $source mutated; preserved logs at $run_dir"
    outcomes="$run_dir/mutants.out/outcomes.json"
    [[ -f "$outcomes" ]] || die "$code produced no outcomes.json (exit $status); logs: $run_dir"

    jq -e --arg mutant "$expected_mutant" '
        .total_mutants == 1
        and .caught == 1
        and .missed == 0
        and .timeout == 0
        and .unviable == 0
        and ([.outcomes[] | select(.scenario == "Baseline" and .summary == "Success")] | length) == 1
        and ([.outcomes[]
            | select(.scenario | type == "object")
            | select(.scenario.Mutant.name == $mutant and .summary == "CaughtMutant")
        ] | length) == 1
    ' "$outcomes" >/dev/null || {
        jq '{total_mutants, caught, missed, timeout, unviable, outcomes: [.outcomes[] | {scenario, summary}]}' "$outcomes" >&2
        die "$code was not exactly one caught mutant (exit $status); logs: $run_dir"
    }
    [[ "$status" == 0 ]] || die "$code cargo-mutants exited $status despite a caught outcome; logs: $run_dir"
    printf 'caught %s; outcomes: %s\n' "$code" "$outcomes"
}

verify_observed() {
    check_matrix
    jq -e '
        all(.rules[]; .observed_outcome == "CaughtMutant")
        and .summary.observed_caught == 20
        and .summary.acceptance_credited_kills == 15
        and .summary.acceptance_complete == false
    ' "$MATRIX" >/dev/null || die "checked-in outcomes are incomplete or overclaim RFC acceptance"
    printf 'recorded outcomes are self-consistent: 20 caught selections; run run-all to observe them again\n'
}

usage() {
    printf '%s\n' \
        "usage: $0 check" \
        "       $0 run <SWIR-...>" \
        "       $0 run-all" \
        "       $0 verify-observed"
}

case ${1:-} in
    check)
        [[ $# == 1 ]] || { usage >&2; exit 2; }
        check_matrix
        ;;
    run)
        [[ $# == 2 ]] || { usage >&2; exit 2; }
        check_matrix
        run_rule "$2"
        ;;
    run-all)
        [[ $# == 1 ]] || { usage >&2; exit 2; }
        check_matrix
        while IFS= read -r code; do
            run_rule "$code"
        done < <(jq -r '.rules | sort_by(.ordinal)[].code' "$MATRIX")
        ;;
    verify-observed)
        [[ $# == 1 ]] || { usage >&2; exit 2; }
        verify_observed
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

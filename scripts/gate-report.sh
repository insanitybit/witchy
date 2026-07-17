#!/usr/bin/env bash
# Read-only merge-gate timing report. This intentionally consumes the existing
# append-only journal and gate logs without taking the queue lock or changing
# coordinator state, so it is safe to run while a gate is live.
#
# Usage:
#   ./scripts/gate-report.sh [--since 24h|7d|all] [--state-dir PATH] [--json]
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(git -C "$here" worktree list --porcelain | head -1 | sed 's/^worktree //')"
. "$here/scripts/state-paths.sh"
state_dir="$(witchy_merge_queue_state_dir "$root")"
since="24h"
json=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --since)
            [ "$#" -ge 2 ] || { echo "gate-report: --since needs 24h, 7d, or all" >&2; exit 2; }
            since="$2"
            shift 2
            ;;
        --state-dir)
            [ "$#" -ge 2 ] || { echo "gate-report: --state-dir needs a path" >&2; exit 2; }
            state_dir="$2"
            shift 2
            ;;
        --json) json=1; shift ;;
        -h | --help)
            sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "gate-report: unknown argument '$1'" >&2; exit 2 ;;
    esac
done

case "$since" in
    all) cutoff=0 ;;
    *h) hours="${since%h}"; case "$hours" in ''|*[!0-9]*) echo "gate-report: invalid --since '$since'" >&2; exit 2;; esac
        cutoff=$(( $(date +%s) - hours * 3600 )) ;;
    *d) days="${since%d}"; case "$days" in ''|*[!0-9]*) echo "gate-report: invalid --since '$since'" >&2; exit 2;; esac
        cutoff=$(( $(date +%s) - days * 86400 )) ;;
    *) echo "gate-report: invalid --since '$since' (want Nh, Nd, or all)" >&2; exit 2 ;;
esac

journal="$state_dir/journal.jsonl"
[ -f "$journal" ] || { echo "gate-report: no journal at $journal" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "gate-report: jq is required" >&2; exit 1; }

tmp="$(mktemp -d "${TMPDIR:-/tmp}/witchy-gate-report-XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
phases="$tmp/phases.jsonl"
attempts="$tmp/attempts.tsv"

# Journaled batch members share a log. Parse every terminal attempt log once.
# Historical logs do not carry fully structured phase events, so discovery is
# the honest residual: test-stage wall time minus Cargo compile and execution.
# New logs carry WITCHY_TIMING records; exact stage values take precedence.
jq -r --argjson cutoff "$cutoff" '
    select((.event=="merged" or .event=="red" or .event=="timeout" or .event=="batch_red")
           and .log != null and .elapsed_s != null
           and ((.ts|fromdateiso8601) >= $cutoff))
    | [.log, .event, ((.gate_elapsed_s // .elapsed_s)|tostring)] | @tsv
' "$journal" | awk -F '\t' '!seen[$1]++' >"$attempts"

# One awk process scans every selected log. Spawning an awk+jq pair per gate
# made a 24-hour report slow enough to contend with the work it was measuring.
LC_ALL=C awk -v meta="$attempts" '
        function reset_values() {
            compile=discovery=execution=test_stage=auxiliary=tests=binaries=""
            exact_tests=exact_fmt=exact_clippy=exact_wasm=exact_book=""
        }
        function duration(s, parts, n, i, value, total) {
            total=0
            n=split(s, parts, " ")
            for (i=1; i<=n; i++) {
                value=parts[i]
                if (value ~ /h$/) { sub(/h$/, "", value); total += value * 3600 }
                else if (value ~ /m$/) { sub(/m$/, "", value); total += value * 60 }
                else if (value ~ /s$/) { sub(/s$/, "", value); total += value }
            }
            return total
        }
        function consume(line, value) {
            gsub(/\033\[[0-9;]*m/, "", line)
            if (line ~ /Finished `test` profile .* in /) {
                value=line; sub(/^.* in /, "", value); compile=duration(value)
            }
            if (line ~ /Starting [0-9]+ tests across [0-9]+ binaries/) {
                value=line; sub(/^.*Starting /, "", value); sub(/^[[:space:]]*/, "", value); split(value, fields, " ")
                tests=fields[1]; binaries=fields[4]
            }
            if (line ~ /Summary \[ *[0-9.]+s\]/) {
                value=line; sub(/^.*Summary \[ */, "", value); sub(/s\].*$/, "", value)
                execution=value
            }
            if (line ~ /\[1\] tests \(workspace\) took [0-9]+s$/) {
                value=line; sub(/^.* took /, "", value); sub(/s$/, "", value); test_stage=value
            }
            if (line ~ /\[[2-9][0-9]*\] .* took [0-9]+s$/) {
                value=line; sub(/^.* took /, "", value); sub(/s$/, "", value)
                if (auxiliary == "") auxiliary=0
                auxiliary += value
            }
            if (line ~ /^WITCHY_TIMING \{/ && line ~ /"status":"green"/) {
                value=line; sub(/^.*"name":"/, "", value); sub(/".*$/, "", value)
                timing_name=value
                value=line; sub(/^.*"elapsed_s":/, "", value); sub(/[,}].*$/, "", value)
                if (timing_name ~ /^tests \(workspace/) exact_tests=value
                else if (timing_name ~ /^witchy fmt /) exact_fmt=value
                else if (timing_name == "clippy (deny warnings)") exact_clippy=value
                else if (timing_name == "wasm playground build") exact_wasm=value
                else if (timing_name == "runnable book (browser)") exact_book=value
            }
        }
        function emit(logfile, outcome, elapsed) {
            if (exact_tests != "") test_stage=exact_tests
            if (test_stage != "" && compile != "" && execution != "") {
                discovery=test_stage-compile-execution
                if (discovery < 0) discovery=0
            }
            printf "%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s\n", logfile, outcome, elapsed, compile, discovery, execution, test_stage, auxiliary, tests, binaries, exact_tests, exact_fmt, exact_clippy, exact_wasm, exact_book
        }
        BEGIN {
            while ((getline row < meta) > 0) {
                split(row, fields, "\t")
                logfile=fields[1]
                gate_outcome=fields[2]
                gate_elapsed=fields[3]
                if ((getline probe < logfile) < 0) continue
                close(logfile)
                reset_values()
                while ((getline line < logfile) > 0) consume(line)
                close(logfile)
                emit(logfile, gate_outcome, gate_elapsed)
            }
            close(meta)
        }
    ' | jq -Rc '
        def number_or_null: if .=="" then null else tonumber end;
        split("|") as $f
        | {log:$f[0], outcome:$f[1], elapsed_s:($f[2]|tonumber),
           compile_s:($f[3]|number_or_null), discovery_estimate_s:($f[4]|number_or_null),
           execution_s:($f[5]|number_or_null), test_stage_s:($f[6]|number_or_null),
           auxiliary_s:($f[7]|number_or_null), tests:($f[8]|number_or_null),
           binaries:($f[9]|number_or_null), exact_tests_s:($f[10]|number_or_null),
           fmt_s:($f[11]|number_or_null), clippy_s:($f[12]|number_or_null),
           wasm_s:($f[13]|number_or_null), book_s:($f[14]|number_or_null)}' >"$phases"

generated="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
report="$tmp/report.json"
jq -sn \
    --arg generated "$generated" --arg since "$since" --arg state_dir "$state_dir" \
    --argjson cutoff "$cutoff" --slurpfile journal "$journal" --slurpfile phases "$phases" '
    def terminal: .event=="merged" or .event=="red" or .event=="timeout" or .event=="batch_red";
    def percentile($p):
        map(select(. != null)) | sort as $values
        | if ($values|length)==0 then null
          else $values[[1, (($values|length)*$p|ceil)] | max - 1]
          end;
    def distribution:
        {count:(map(select(. != null))|length),
         p50:(percentile(0.50)), p90:(percentile(0.90)), max:(map(select(. != null))|max//null)};
    def numeric($value): if $value==null then null else ($value|tonumber) end;
    ($journal | map(select((.ts|fromdateiso8601) >= $cutoff))) as $recent
    | ($recent | map(select(terminal and .log != null and .elapsed_s != null)) | unique_by(.log)) as $attempts
    | ($recent | map(select(.event=="merged" and .log != null and .elapsed_s != null))) as $merged
    | ($merged | unique_by(.log)) as $green
    | ($attempts | map(select(.event!="merged"))) as $failed
    | ([ $journal | group_by(.branch)[] | sort_by(.ts) as $events
          | range(0; $events|length) as $i
          | select($events[$i].event=="merged" and (($events[$i].ts|fromdateiso8601) >= $cutoff))
          | ([range(0;$i) | select($events[.].event=="submitted")] | last) as $submitted
          | select($submitted != null)
          | (($events[$i].ts|fromdateiso8601)-($events[$submitted].ts|fromdateiso8601))
       ]) as $waits
    | {
        schema:2, generated_at:$generated, since:$since, state_dir:$state_dir,
        throughput:{submissions:($recent|map(select(.event=="submitted"))|length),
                    merged_branches:($merged|length), green_gates:($green|length),
                    failed_attempts:($failed|length),
                    branches_per_green_gate:(if ($green|length)==0 then null else (($merged|length)/($green|length)) end),
                    batched_gates:($green|map(select((.batch//"1"|tonumber)>1))|length),
                    failed_gate_minutes:($failed|map(numeric(.gate_elapsed_s // .elapsed_s))|add//0)/60},
        outcomes:{red:($attempts|map(select(.event=="red"))|length),
                  timeout:($attempts|map(select(.event=="timeout"))|length),
                  batch_red:($attempts|map(select(.event=="batch_red"))|length),
                  requeued:($recent|map(select(.event=="requeued"))|length),
                  conflict:($recent|map(select(.event=="conflict"))|length),
                  blocked:($recent|map(select(.event=="blocked"))|length),
                  automatic_retries:(($recent|map(select(.event=="requeued"))|length)
                                     +($attempts|map(select(.event=="batch_red"))|length))},
        queue_wait_s:($waits|distribution),
        attempt_s:($attempts|map(numeric(.attempt_elapsed_s // .elapsed_s))|distribution),
        gate_s:($attempts|map(numeric(.gate_elapsed_s // .elapsed_s))|distribution),
        attempt_phases_s:{lock_wait:($attempts|map(numeric(.lock_wait_s))|distribution),
                          prepare:($attempts|map(numeric(.prepare_elapsed_s))|distribution),
                          preflight:($attempts|map(numeric(.preflight_elapsed_s))|distribution),
                          gate:($attempts|map(numeric(.gate_elapsed_s // .elapsed_s))|distribution),
                          landing:($attempts|map(numeric(.landing_elapsed_s))|distribution)},
        phases_s:{compile:($phases|map(.compile_s)|distribution),
                  discovery_estimate:($phases|map(.discovery_estimate_s)|distribution),
                  execution:($phases|map(.execution_s)|distribution),
                  test_stage:($phases|map(.test_stage_s)|distribution),
                  auxiliary:($phases|map(.auxiliary_s)|distribution)},
        structured_phases_s:{tests:($phases|map(.exact_tests_s)|distribution),
                             fmt:($phases|map(.fmt_s)|distribution),
                             clippy:($phases|map(.clippy_s)|distribution),
                             wasm:($phases|map(.wasm_s)|distribution),
                             book:($phases|map(.book_s)|distribution)},
        suite:{tests:($phases|map(.tests)|distribution), binaries:($phases|map(.binaries)|distribution)}
      }
' >"$report"

if [ "$json" -eq 1 ]; then
    jq . "$report"
    exit 0
fi

jq -r '
    def value($v; $suffix): if $v==null then "n/a" else (($v|round|tostring)+$suffix) end;
    def decimal($v): if $v==null then "n/a" else ((($v*100|round)/100)|tostring) end;
    "Gate report (last \(.since); generated \(.generated_at))",
    "",
    "Throughput",
    "  submissions:             \(.throughput.submissions)",
    "  merged branches/gates:   \(.throughput.merged_branches)/\(.throughput.green_gates)",
    "  branches per green gate: \(decimal(.throughput.branches_per_green_gate))",
    "  batched green gates:      \(.throughput.batched_gates)",
    "  failed attempts/minutes:  \(.throughput.failed_attempts)/\(value(.throughput.failed_gate_minutes; "m"))",
    "",
    "Latency (p50 / p90 / max)",
    "  queue wait:              \(value(.queue_wait_s.p50; "s")) / \(value(.queue_wait_s.p90; "s")) / \(value(.queue_wait_s.max; "s"))",
    "  coordinator attempt:     \(value(.attempt_s.p50; "s")) / \(value(.attempt_s.p90; "s")) / \(value(.attempt_s.max; "s"))",
    "  actual gate:             \(value(.gate_s.p50; "s")) / \(value(.gate_s.p90; "s")) / \(value(.gate_s.max; "s"))",
    "",
    "Coordinator attempt phases (p50 / p90; exact where instrumented)",
    "  lock wait (n=\(.attempt_phases_s.lock_wait.count)): \(value(.attempt_phases_s.lock_wait.p50; "s")) / \(value(.attempt_phases_s.lock_wait.p90; "s"))",
    "  preparation (n=\(.attempt_phases_s.prepare.count)): \(value(.attempt_phases_s.prepare.p50; "s")) / \(value(.attempt_phases_s.prepare.p90; "s"))",
    "  locked preflight (n=\(.attempt_phases_s.preflight.count)): \(value(.attempt_phases_s.preflight.p50; "s")) / \(value(.attempt_phases_s.preflight.p90; "s"))",
    "  gate (n=\(.attempt_phases_s.gate.count)):      \(value(.attempt_phases_s.gate.p50; "s")) / \(value(.attempt_phases_s.gate.p90; "s"))",
    "  landing/finalize (n=\(.attempt_phases_s.landing.count)): \(value(.attempt_phases_s.landing.p50; "s")) / \(value(.attempt_phases_s.landing.p90; "s"))",
    "",
    "Gate phase wall time (p50 / p90; parsed logs)",
    "  compile (n=\(.phases_s.compile.count)):                \(value(.phases_s.compile.p50; "s")) / \(value(.phases_s.compile.p90; "s"))",
    "  discovery/overhead est (n=\(.phases_s.discovery_estimate.count)): \(value(.phases_s.discovery_estimate.p50; "s")) / \(value(.phases_s.discovery_estimate.p90; "s"))",
    "  test execution (n=\(.phases_s.execution.count)):         \(value(.phases_s.execution.p50; "s")) / \(value(.phases_s.execution.p90; "s"))",
    "  full test stage (n=\(.phases_s.test_stage.count)):        \(value(.phases_s.test_stage.p50; "s")) / \(value(.phases_s.test_stage.p90; "s"))",
    "  auxiliary stages (n=\(.phases_s.auxiliary.count)):       \(value(.phases_s.auxiliary.p50; "s")) / \(value(.phases_s.auxiliary.p90; "s"))",
    "",
    "Structured stage duration (p50 / p90; exact where instrumented)",
    "  tests (n=\(.structured_phases_s.tests.count)):   \(value(.structured_phases_s.tests.p50; "s")) / \(value(.structured_phases_s.tests.p90; "s"))",
    "  fmt (n=\(.structured_phases_s.fmt.count)):     \(value(.structured_phases_s.fmt.p50; "s")) / \(value(.structured_phases_s.fmt.p90; "s"))",
    "  clippy (n=\(.structured_phases_s.clippy.count)):  \(value(.structured_phases_s.clippy.p50; "s")) / \(value(.structured_phases_s.clippy.p90; "s"))",
    "  wasm (n=\(.structured_phases_s.wasm.count)):    \(value(.structured_phases_s.wasm.p50; "s")) / \(value(.structured_phases_s.wasm.p90; "s"))",
    "  book (n=\(.structured_phases_s.book.count)):    \(value(.structured_phases_s.book.p50; "s")) / \(value(.structured_phases_s.book.p90; "s"))",
    "",
    "Outcomes",
    "  red / timeout / batch-red: \(.outcomes.red) / \(.outcomes.timeout) / \(.outcomes.batch_red)",
    "  requeued / conflict / blocked: \(.outcomes.requeued) / \(.outcomes.conflict) / \(.outcomes.blocked)",
    "  automatic retry events: \(.outcomes.automatic_retries)",
    "",
    "Discovery is estimated as test-stage wall time minus Cargo compile and nextest execution."
' "$report"

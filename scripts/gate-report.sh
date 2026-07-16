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
state_dir="$root/scratch/merge-queue"
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
# Existing logs do not carry fully structured phase events, so discovery is the
# honest residual: test-stage wall time minus Cargo compile and test execution.
jq -r --argjson cutoff "$cutoff" '
    select((.event=="merged" or .event=="red" or .event=="timeout" or .event=="batch_red")
           and .log != null and .elapsed_s != null
           and ((.ts|fromdateiso8601) >= $cutoff))
    | [.log, .event, (.elapsed_s|tostring)] | @tsv
' "$journal" | awk -F '\t' '!seen[$1]++' >"$attempts"

# One awk process scans every selected log. Spawning an awk+jq pair per gate
# made a 24-hour report slow enough to contend with the work it was measuring.
LC_ALL=C awk -v meta="$attempts" '
        function reset_values() {
            compile=discovery=execution=test_stage=auxiliary=tests=binaries=""
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
        }
        function emit(logfile, outcome, elapsed) {
            if (test_stage != "" && compile != "" && execution != "") {
                discovery=test_stage-compile-execution
                if (discovery < 0) discovery=0
            }
            printf "%s|%s|%s|%s|%s|%s|%s|%s|%s|%s\n", logfile, outcome, elapsed, compile, discovery, execution, test_stage, auxiliary, tests, binaries
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
           binaries:($f[9]|number_or_null)}' >"$phases"

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
        schema:1, generated_at:$generated, since:$since, state_dir:$state_dir,
        throughput:{submissions:($recent|map(select(.event=="submitted"))|length),
                    merged_branches:($merged|length), green_gates:($green|length),
                    failed_attempts:($failed|length),
                    branches_per_green_gate:(if ($green|length)==0 then null else (($merged|length)/($green|length)) end),
                    batched_gates:($green|map(select((.batch//"1"|tonumber)>1))|length),
                    failed_gate_minutes:($failed|map(.elapsed_s|tonumber)|add//0)/60},
        outcomes:{red:($attempts|map(select(.event=="red"))|length),
                  timeout:($attempts|map(select(.event=="timeout"))|length),
                  batch_red:($attempts|map(select(.event=="batch_red"))|length)},
        queue_wait_s:($waits|distribution),
        gate_s:($attempts|map(.elapsed_s|tonumber)|distribution),
        phases_s:{compile:($phases|map(.compile_s)|distribution),
                  discovery_estimate:($phases|map(.discovery_estimate_s)|distribution),
                  execution:($phases|map(.execution_s)|distribution),
                  test_stage:($phases|map(.test_stage_s)|distribution),
                  auxiliary:($phases|map(.auxiliary_s)|distribution)},
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
    "  gate:                    \(value(.gate_s.p50; "s")) / \(value(.gate_s.p90; "s")) / \(value(.gate_s.max; "s"))",
    "",
    "Gate phase wall time (p50 / p90; parsed logs)",
    "  compile (n=\(.phases_s.compile.count)):                \(value(.phases_s.compile.p50; "s")) / \(value(.phases_s.compile.p90; "s"))",
    "  discovery/overhead est (n=\(.phases_s.discovery_estimate.count)): \(value(.phases_s.discovery_estimate.p50; "s")) / \(value(.phases_s.discovery_estimate.p90; "s"))",
    "  test execution (n=\(.phases_s.execution.count)):         \(value(.phases_s.execution.p50; "s")) / \(value(.phases_s.execution.p90; "s"))",
    "  full test stage (n=\(.phases_s.test_stage.count)):        \(value(.phases_s.test_stage.p50; "s")) / \(value(.phases_s.test_stage.p90; "s"))",
    "  auxiliary stages (n=\(.phases_s.auxiliary.count)):       \(value(.phases_s.auxiliary.p50; "s")) / \(value(.phases_s.auxiliary.p90; "s"))",
    "",
    "Outcomes",
    "  red / timeout / batch-red: \(.outcomes.red) / \(.outcomes.timeout) / \(.outcomes.batch_red)",
    "",
    "Discovery is estimated as test-stage wall time minus Cargo compile and nextest execution."
' "$report"

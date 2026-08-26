#!/usr/bin/env bash
# Read-only merge-gate timing report. This intentionally consumes the existing
# append-only journal and gate logs without taking the queue lock or changing
# coordinator state, so it is safe to run while a gate is live.
#
# Usage:
#   ./scripts/gate-report.sh [--since 24h|7d|all] [--state-dir PATH] [--json]
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(git -C "$here" worktree list --porcelain | sed -n '1p' | sed 's/^worktree //')"
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
            saw_stage_marker=in_workspace_tests=0
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
            # A gate can invoke nextest again for queue infrastructure, fuzz,
            # or another isolated shard. Attribute unstructured Cargo/nextest
            # lines only to the primary workspace stage; otherwise the last
            # auxiliary invocation silently overwrites the compile, execution,
            # test-count, and binary-count figures. Historical logs without
            # stage markers retain the old whole-log fallback.
            if (line ~ /^==> \[[0-9]+\] /) {
                saw_stage_marker=1
                in_workspace_tests=(line ~ /^==> \[1\] tests \(workspace\)/)
            }
            if ((!saw_stage_marker || in_workspace_tests) && line ~ /Finished `test` profile .* in /) {
                value=line; sub(/^.* in /, "", value); compile=duration(value)
            }
            if ((!saw_stage_marker || in_workspace_tests) && line ~ /Starting [0-9]+ tests across [0-9]+ binaries/) {
                value=line; sub(/^.*Starting /, "", value); sub(/^[[:space:]]*/, "", value); split(value, fields, " ")
                tests=fields[1]; binaries=fields[4]
            }
            if ((!saw_stage_marker || in_workspace_tests) && line ~ /Summary \[ *[0-9.]+s\]/) {
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
                else if (timing_name ~ /^clippy /) exact_clippy=value
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
    # Load per core at gate start. Absent on attempts journalled before the
    # contention signal landed, and on hosts where the probe failed -- both
    # must read as "no sample", never as zero load.
    def load_per_core:
        if (.gate_load_start//null)==null or (.host_ncpu//null)==null
        then null
        else ((.host_ncpu|tonumber) as $n
              | if $n==0 then null else ((.gate_load_start|tonumber)/$n) end)
        end;
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
        schema:3, generated_at:$generated, since:$since, state_dir:$state_dir,
        throughput:{submissions:($recent|map(select(.event=="submitted"))|length),
                    merged_branches:($merged|length), green_gates:($green|length),
                    failed_attempts:($failed|length),
                    branches_per_green_gate:(if ($green|length)==0 then null else (($merged|length)/($green|length)) end),
                    batched_gates:($green|map(select((.batch//"1"|tonumber)>1))|length),
                    failed_gate_minutes:(($failed|map(numeric(.gate_elapsed_s // .elapsed_s))|add // 0) / 60)},
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
        suite:{tests:($phases|map(.tests)|distribution), binaries:($phases|map(.binaries)|distribution)},
        contention:(($attempts|map(select(load_per_core != null))) as $c
          | {samples:($c|length),
             load_per_core:($c|map(load_per_core)|distribution),
             buckets:([{name:"idle (<1x cores)",  sel:($c|map(select(load_per_core < 1)))},
                       {name:"busy (1-3x)",       sel:($c|map(select(load_per_core >= 1 and load_per_core < 3)))},
                       {name:"saturated (>=3x)",  sel:($c|map(select(load_per_core >= 3)))}]
                      | map({name:.name, n:(.sel|length),
                             gate_p50:(.sel|map(numeric(.gate_elapsed_s // .elapsed_s))|distribution|.p50),
                             gate_p90:(.sel|map(numeric(.gate_elapsed_s // .elapsed_s))|distribution|.p90),
                             red_rate:(if (.sel|length)==0 then null
                                       else ((.sel|map(select(.event!="merged"))|length)/(.sel|length)) end)}))}),
        # RFC-0159. Attribution and change shape, recorded at submit time.
        # "unset" is a first-class value: it must stay visible so adoption is
        # measurable rather than silently defaulting to a wrong name.
        attribution:(($recent|map(select(.event=="submitted" or .event=="resubmitted"))) as $subs
          | {submissions:($subs|length),
             actor_adoption:(if ($subs|length)==0 then null
                             else (($subs|map(select((.actor//"unset")!="unset"))|length)/($subs|length)) end),
             trailer_adoption:(if ($subs|length)==0 then null
                               else (($subs|map(select(.trailer_present=="true"))|length)/($subs|length)) end),
             presubmit_receipt_rate:(if ($subs|length)==0 then null
                                     else (($subs|map(select(.presubmit_sha_match=="true"))|length)/($subs|length)) end),
             by_actor:($subs|group_by(.actor//"unset")
                       |map({actor:(.[0].actor//"unset"), submissions:length})|sort_by(-.submissions)),
             by_touches:($subs|map(select((.touches//"")!=""))|group_by(.touches)
                         |map({touches:.[0].touches, submissions:length})|sort_by(-.submissions))}),
        red_causes:($failed|group_by(.red_class//"unclassified")
                    |map({red_class:(.[0].red_class//"unclassified"), n:length,
                          gate_minutes:((map(numeric(.gate_elapsed_s // .elapsed_s))|add // 0)/60)})
                    |sort_by(-.gate_minutes)),
        # The decomposition that makes the latency picture legible: time a
        # change spent being worked on versus time it spent waiting for a
        # coordinator to exist. The tail is dominated by the second, which no
        # amount of gate-speed work can touch.
        work_vs_idle_s:(([ $journal | group_by(.branch)[] | sort_by(.ts) as $events
              | range(0; $events|length) as $i
              | select($events[$i].event=="merged" and (($events[$i].ts|fromdateiso8601) >= $cutoff))
              | ([range(0;$i) | select($events[.].event=="submitted")] | last) as $submitted
              | select($submitted != null)
              | (($events[$i].ts|fromdateiso8601)-($events[$submitted].ts|fromdateiso8601)) as $total
              | ([range($submitted;$i+1) | $events[.] | numeric(.attempt_elapsed_s) // 0] | add) as $work
              | {total:$total, work:$work, idle:(if $total>$work then $total-$work else 0 end)}
           ]) as $rows
          | {time_to_land:($rows|map(.total)|distribution),
             attempt_work:($rows|map(.work)|distribution),
             idle_in_queue:($rows|map(.idle)|distribution)}),
        first_pass_clean:(([ $journal | group_by(.change_id//.branch)[] | sort_by(.ts) as $events
              | select($events|map(select(.event=="merged"))|length > 0)
              | select(($events|map(select(.event=="merged"))|last|.ts|fromdateiso8601) >= $cutoff)
              | (($events|map(select(.event=="red" or .event=="batch_red" or .event=="timeout"))|length)==0)
           ]) as $clean
          | if ($clean|length)==0 then null
            else (($clean|map(select(.))|length)/($clean|length)) end),
        # Uptime is computable only because the coordinator now writes a
        # heartbeat when nothing is happening. Before that, "no coordinator"
        # and "no queue" were the same silence.
        coordinator:(($recent|map(select(.event=="heartbeat"))) as $beats
          | {heartbeats:($beats|length),
             covered_s:(($beats|length) * 300),
             window_s:((now|floor) - $cutoff),
             uptime_ratio:(if ((now|floor) - $cutoff) <= 0 then null
                           else ((($beats|length) * 300) / ((now|floor) - $cutoff)) end)})
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
    (if .contention.samples == 0 then
        "Host contention: no samples yet (attempts predate the contention signal)."
     else
        ("Host contention at gate start (load per core; p50 / p90 / max)\n"
         + "  load per core (n=\(.contention.samples)): \(decimal(.contention.load_per_core.p50)) / \(decimal(.contention.load_per_core.p90)) / \(decimal(.contention.load_per_core.max))\n"
         + "  gate p50 / p90 and failure rate by load bucket:\n"
         + (.contention.buckets | map("    \(.name): n=\(.n)  gate \(value(.gate_p50; "s")) / \(value(.gate_p90; "s"))  failed \(if .red_rate==null then "n/a" else ((.red_rate*100|round|tostring)+"%") end)") | join("\n")))
     end),
    "",
    "Work vs idle for changes that landed (p50 / p90)",
    "  time to land:            \(value(.work_vs_idle_s.time_to_land.p50; "s")) / \(value(.work_vs_idle_s.time_to_land.p90; "s"))",
    "  attempt work:            \(value(.work_vs_idle_s.attempt_work.p50; "s")) / \(value(.work_vs_idle_s.attempt_work.p90; "s"))",
    "  idle in queue:           \(value(.work_vs_idle_s.idle_in_queue.p50; "s")) / \(value(.work_vs_idle_s.idle_in_queue.p90; "s"))",
    "  first-pass-clean rate:   \(if .first_pass_clean==null then "n/a" else ((.first_pass_clean*100|round|tostring)+"%") end)",
    "  coordinator uptime:      \(if .coordinator.uptime_ratio==null then "n/a" else ((.coordinator.uptime_ratio*100|round|tostring)+"% (\(.coordinator.heartbeats) heartbeats)") end)",
    "",
    "Pre-submit evidence and attribution (n=\(.attribution.submissions) submissions)",
    "  green shard receipt:     \(if .attribution.presubmit_receipt_rate==null then "n/a" else ((.attribution.presubmit_receipt_rate*100|round|tostring)+"%") end)",
    "  actor set (not \"unset\"): \(if .attribution.actor_adoption==null then "n/a" else ((.attribution.actor_adoption*100|round|tostring)+"%") end)",
    "  Co-Authored-By trailer:  \(if .attribution.trailer_adoption==null then "n/a" else ((.attribution.trailer_adoption*100|round|tostring)+"%") end)",
    (if (.red_causes|length)==0 then "" else
        ("\nFailed gate time by cause\n"
         + (.red_causes | map("  \(.red_class): n=\(.n)  \(value(.gate_minutes; "m"))") | join("\n")))
     end),
    "",
    "Discovery is estimated as test-stage wall time minus Cargo compile and nextest execution.",
    "A gate is only comparable to another at similar load per core; see the contention buckets.",
    "A green shard receipt certifies the AUTHOR'"'"'S tree, not the rebased tree the gate runs."
' "$report"

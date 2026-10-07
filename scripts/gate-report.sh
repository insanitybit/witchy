#!/usr/bin/env bash
# Read-only merge-gate timing report. This intentionally consumes the existing
# append-only journal and gate logs without taking the queue lock or changing
# coordinator state, so it is safe to run while a gate is live.
#
# Usage:
#   ./scripts/gate-report.sh [--since 24h|7d|all|UTC-ISO-8601] [--state-dir PATH] [--json]
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
            [ "$#" -ge 2 ] || { echo "gate-report: --since needs 24h, 7d, all, or UTC ISO-8601" >&2; exit 2; }
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
    *)
        case "$since" in
            ????-??-??T??:??:??Z)
                if [[ "$(uname -s)" == "Darwin" ]]; then
                    cutoff="$(date -j -u -f '%Y-%m-%dT%H:%M:%SZ' "$since" '+%s' 2>/dev/null || true)"
                else
                    cutoff="$(date -u -d "$since" '+%s' 2>/dev/null || true)"
                fi
                case "$cutoff" in
                    ''|*[!0-9]*) echo "gate-report: invalid --since '$since' (want UTC ISO-8601, for example 2026-09-14T03:39:55Z)" >&2; exit 2 ;;
                esac
                ;;
            *) echo "gate-report: invalid --since '$since' (want Nh, Nd, all, or UTC ISO-8601)" >&2; exit 2 ;;
        esac
        ;;
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
    | [.log, .event, ((.gate_elapsed_s // .elapsed_s)|tostring),
       (.test_selector // "legacy"), (.nextest_scheduled_tests // ""),
       (.nextest_executed_tests // ""), (.nextest_passed_tests // ""),
       (.nextest_failed_tests // ""), (.nextest_skipped_tests // ""),
       (.nextest_binaries // "")] | @tsv
' "$journal" | awk -F '\t' '!seen[$1]++' >"$attempts"

# One awk process scans every selected log. Spawning an awk+jq pair per gate
# made a 24-hour report slow enough to contend with the work it was measuring.
LC_ALL=C awk -v meta="$attempts" '
        function reset_values() {
            compile=discovery=execution=test_stage=auxiliary=tests=binaries=""
            executed=passed=failed=skipped=""
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
            if ((!saw_stage_marker || in_workspace_tests) && line ~ /Starting [0-9]+ tests? across [0-9]+ binar(y|ies)/) {
                value=line; sub(/^.*Starting /, "", value); sub(/^[[:space:]]*/, "", value); split(value, fields, " ")
                tests=fields[1]; binaries=fields[4]
            }
            if ((!saw_stage_marker || in_workspace_tests) && line ~ /Summary \[ *[0-9.]+s\]/) {
                value=line; sub(/^.*Summary \[ */, "", value); sub(/s\].*$/, "", value)
                execution=value
                value=line; sub(/^.*s\] /, "", value); n=split(value, summary_fields, " ")
                run=summary_fields[1]; sub(/\/.*/, "", run)
                if (run ~ /^[0-9]+$/) executed=run
                for (i=2; i<=n; i++) {
                    label=summary_fields[i]; gsub(/[:,]/, "", label)
                    number=summary_fields[i-1]; gsub(/[^0-9]/, "", number)
                    if (number == "") continue
                    if (label == "passed") passed=number
                    else if (label == "failed") failed=number
                    else if (label == "skipped") skipped=number
                }
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
            printf "%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s\n", logfile, outcome, elapsed, selector, compile, discovery, execution, test_stage, auxiliary, tests, executed, passed, failed, skipped, binaries, exact_tests, exact_fmt, exact_clippy, exact_wasm, exact_book
        }
        BEGIN {
            while ((getline row < meta) > 0) {
                split(row, metadata, "\t")
                logfile=metadata[1]
                gate_outcome=metadata[2]
                gate_elapsed=metadata[3]
                selector=metadata[4]
                if ((getline probe < logfile) < 0) continue
                close(logfile)
                reset_values()
                while ((getline line < logfile) > 0) consume(line)
                close(logfile)
                if (metadata[5] != "") tests=metadata[5]
                if (metadata[6] != "") executed=metadata[6]
                if (metadata[7] != "") passed=metadata[7]
                if (metadata[8] != "") failed=metadata[8]
                if (metadata[9] != "") skipped=metadata[9]
                if (metadata[10] != "") binaries=metadata[10]
                emit(logfile, gate_outcome, gate_elapsed)
            }
            close(meta)
        }
    ' | jq -Rc '
        def number_or_null: if .=="" then null else tonumber end;
        split("|") as $f
        | {log:$f[0], outcome:$f[1], elapsed_s:($f[2]|tonumber), selector:$f[3],
           compile_s:($f[4]|number_or_null), discovery_estimate_s:($f[5]|number_or_null),
           execution_s:($f[6]|number_or_null), test_stage_s:($f[7]|number_or_null),
           auxiliary_s:($f[8]|number_or_null), scheduled_tests:($f[9]|number_or_null),
           executed_tests:($f[10]|number_or_null), passed_tests:($f[11]|number_or_null),
           failed_tests:($f[12]|number_or_null), skipped_tests:($f[13]|number_or_null),
           binaries:($f[14]|number_or_null), exact_tests_s:($f[15]|number_or_null),
           fmt_s:($f[16]|number_or_null), clippy_s:($f[17]|number_or_null),
           wasm_s:($f[18]|number_or_null), book_s:($f[19]|number_or_null)}' >"$phases"

generated="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
report="$tmp/report.json"
jq -sn \
    --arg generated "$generated" --arg since "$since" --arg state_dir "$state_dir" \
    --argjson cutoff "$cutoff" --slurpfile journal "$journal" --slurpfile phases "$phases" '
    def gate_terminal: .event=="merged" or .event=="red" or .event=="timeout" or .event=="batch_red";
    def gate_failure: .event=="red" or .event=="timeout" or .event=="batch_red";
    def percentile($p):
        map(select(. != null)) | sort as $values
        | if ($values|length)==0 then null
          else $values[[1, (($values|length)*$p|ceil)] | max - 1]
          end;
    def distribution:
        {count:(map(select(. != null))|length),
         p50:(percentile(0.50)), p90:(percentile(0.90)), max:(map(select(. != null))|max//null)};
    def gate_profile:
        {samples:length,
         gate_s:(map(.elapsed_s)|distribution),
         compile_s:(map(.compile_s)|distribution),
         execution_s:(map(.execution_s)|distribution),
         executed_tests:(map(.executed_tests)|distribution)};
    def numeric($value): if $value==null or $value=="" then null else ($value|tonumber) end;
    def delta_if_numeric($left; $right):
        if $left == null or $right == null then null else $left - $right end;
    def attempt_elapsed: numeric(.attempt_elapsed_s // .elapsed_s);
    def attempt_lock_wait: (numeric(.lock_wait_s) // 0);
    def attempt_work:
        (attempt_elapsed - attempt_lock_wait) as $work
        | if $work < 0 then 0 else $work end;
    # A successful batch writes one `merged` event per member. Those events
    # describe one physical coordinator attempt and share its gate log. Keep
    # branch throughput per member, but count attempt time only once. The
    # fallback covers older timing-bearing batch events that lack a log.
    def attempt_key:
        if ((.log//"")|length)>0 then "log:"+.log
        elif ((.batch//"1"|tonumber)>1) then
            ["batch", (.sha//"unknown"), (.attempt_elapsed_s//.elapsed_s//""),
             (.prepare_elapsed_s//""), (.lock_wait_s//""),
             (.preflight_elapsed_s//""), (.gate_elapsed_s//.elapsed_s//""),
             (.landing_elapsed_s//"")] | map(tostring) | join(":")
        else ["event", .ts, .event, (.branch//"")] | join(":")
        end;
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
    | ($recent | map(select(gate_terminal and (.attempt_elapsed_s != null or .elapsed_s != null)))) as $terminal_events
    | ($terminal_events | unique_by(attempt_key)) as $attempts
    | ($recent | map(select(.event=="merged" and .log != null and .elapsed_s != null))) as $merged
    | ($merged | unique_by(.log)) as $green
    # Conflicts, requeues, and blocks are coordinator outcomes, but no gate ran;
    # keep them out of failed-attempt counts and failed gate time.
    | ($attempts | map(select(gate_failure))) as $failed
    | ([ $journal | map(select(.event=="merged" and ((.ts|fromdateiso8601) >= $cutoff)))[] as $landed
          | ($landed.attempt_id//"") as $landed_attempt
          | ($landed.change_id//"") as $landed_change
          | ($landed.submitted_sha//"") as $landed_sha
          | ([ $journal[]
                | select(.ts <= $landed.ts and .branch==$landed.branch)
                | select(.event=="submitted" or .event=="resubmitted")
                # A branch may be resubmitted while an older snapshot is in a
                # live gate. Match that landed snapshot exactly when current
                # journal identifiers exist; chronology is only for legacy
                # records that have no exact attempt or change/SHA identity.
                | select(if ($landed_attempt|length)>0 then
                             ((.attempt_id//"")==$landed_attempt)
                         elif ($landed_change|length)>0 and ($landed_sha|length)>0 then
                             ((.change_id//"")==$landed_change
                              and (.submitted_sha//"")==$landed_sha)
                         else true end)
             ] | last) as $submitted
          | select($submitted != null)
          | (($landed.ts|fromdateiso8601)-($submitted.ts|fromdateiso8601)) as $total
          | ([ $journal[]
                | select(.ts >= $submitted.ts and .ts <= $landed.ts)
                | select(gate_terminal and (.attempt_elapsed_s != null or .elapsed_s != null))
                | select(.branch==$landed.branch
                         or (((.members//"")|split(" ")|index($landed.branch)) != null))
             ] | unique_by(attempt_key)
                | map(numeric(.attempt_elapsed_s // .elapsed_s)) | add//0) as $attempt
          | {branch:$landed.branch, total:$total, attempt:$attempt,
             scheduling:(if $total>$attempt then $total-$attempt else 0 end)}
       ]) as $landed_rows
    | ([ $journal | map(select(.event=="merged" and ((.ts|fromdateiso8601) >= $cutoff)))[] as $landed
          | ($landed.change_id//"") as $change_id
          | ([ $journal[]
                | select(.branch==$landed.branch and .ts < $landed.ts and .event=="merged")
                | .ts
             ] | max//"") as $previous_land
          | ([ $journal[]
                | select(.branch==$landed.branch and .ts <= $landed.ts)
                | select(.event=="submitted" or .event=="resubmitted")
                | select(if ($change_id|length)>0 then (.change_id==$change_id)
                         else .ts>$previous_land end)
                | .ts
             ] | min//"") as $change_start
          | (if ($change_start|length)==0 then [] else
               [ $journal[]
                 | select(.ts >= $change_start and .ts <= $landed.ts)
                 | select(gate_failure)
                 | select((if ($change_id|length)>0 then (.change_id==$change_id)
                           else .branch==$landed.branch end)
                          or (((.members//"")|split(" ")|index($landed.branch)) != null))
               ] | unique_by(attempt_key)
             end) as $prior_failures
          | {branch:$landed.branch, failures:($prior_failures|length),
             failed_attempt_s:($prior_failures|map(attempt_elapsed)|add//0),
             failed_work_s:($prior_failures|map(attempt_work)|add//0),
             failed_lock_wait_s:($prior_failures|map(attempt_lock_wait)|add//0)}
       ]) as $rework_rows
    | ($phases | map(. as $phase
        | . + {load_per_core:((($attempts
            | map(select(.log==$phase.log)) | .[0]) // {}) | load_per_core)}
        | . + {
            executed_test_band:(if .executed_tests==null then "unknown"
                                elif .executed_tests<100 then "<100"
                                elif .executed_tests<300 then "100-299"
                                elif .executed_tests<600 then "300-599"
                                else "600+" end),
            binary_band:(if .binaries==null then "unknown"
                         elif .binaries<4 then "1-3"
                         elif .binaries<12 then "4-11"
                         else "12+" end),
            load_band:(if .load_per_core==null then "unknown"
                       elif .load_per_core<1 then "idle (<1/core)"
                       elif .load_per_core<3 then "busy (1-3/core)"
                       else "saturated (>=3/core)" end)
          })) as $cohort_phases
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
        # Retained for schema-3 consumers. This is final submit/resubmit through
        # landing, including the coordinator attempt; it is not pure queue wait.
        queue_wait_s:($landed_rows|map(.total)|distribution),
        attempt_s:($attempts|map(numeric(.attempt_elapsed_s // .elapsed_s))|distribution),
        gate_s:($attempts|map(numeric(.gate_elapsed_s // .elapsed_s))|distribution),
        attempt_phases_s:{lock_wait:($attempts|map(numeric(.lock_wait_s))|distribution),
                          prepare:($attempts|map(numeric(.prepare_elapsed_s))|distribution),
                          preflight:($attempts|map(numeric(.preflight_elapsed_s))|distribution),
                          gate:($attempts|map(numeric(.gate_elapsed_s // .elapsed_s))|distribution),
                          landing:($attempts|map(numeric(.landing_elapsed_s))|distribution)},
        development_latency:{
          coverage:{edit_to_submit:{available:false,
                                    reason:"the merge-queue journal does not record first-edit timestamps"},
                    final_submit_to_land:{available:true, samples:($landed_rows|length),
                                          merged_events:($recent|map(select(.event=="merged"))|length),
                                          source:"matching attempt_id; change_id plus submitted_sha fallback; branch chronology only for legacy records"},
                    scheduling_wait:{available:true, samples:($landed_rows|length),
                                     source:"final submit-to-land minus dequeue-to-terminal attempt",
                                     limitation:"pre-dequeue idle can include coordinator unavailability or machine sleep; the journal cannot distinguish them"},
                    attempt_phases:{available:true, unique_attempts:($attempts|length),
                                    instrumented:($attempts|map(select(.attempt_timing_schema != null))|length)},
                    gate_outcomes:{source:"merged, red, timeout, and batch_red only",
                                   limitation:"conflict, requeued, and blocked are counted as outcomes but never as gate attempts or failed gate time"},
                    concurrency:{available:($attempts|map(select(load_per_core != null))|length>0),
                                 samples:($attempts|map(select(load_per_core != null))|length),
                                 source:"host load per core at gate start"}},
          shared_batch_dedup:{terminal_events:($terminal_events|length),
                              unique_attempts:($attempts|length),
                              duplicate_member_events:(($terminal_events|length)-($attempts|length)),
                              key:"shared gate log; timing tuple fallback for legacy logless batches"},
          final_submit_to_land_s:($landed_rows|map(.total)|distribution),
          scheduling_wait_s:($landed_rows|map(.scheduling)|distribution),
          rework:{landed_changes:($rework_rows|length),
                  affected_changes:($rework_rows|map(select(.failures>0))|length),
                  failed_attempts_per_affected_change:($rework_rows|map(select(.failures>0)|.failures)|distribution),
                  failed_attempt_s_per_affected_change:($rework_rows|map(select(.failures>0)|.failed_attempt_s)|distribution),
                  failed_work_s_per_affected_change:($rework_rows|map(select(.failures>0)|.failed_work_s)|distribution),
                  failed_lock_wait_s_per_affected_change:($rework_rows|map(select(.failures>0)|.failed_lock_wait_s)|distribution),
                  unique_failed_attempts:($failed|length),
                  unique_failed_attempt_s_total:($failed|map(attempt_elapsed)|add//0),
                  unique_failed_work_s_total:($failed|map(attempt_work)|add//0),
                  unique_failed_lock_wait_s_total:($failed|map(attempt_lock_wait)|add//0),
                  attribution:"batch failures affect every branch named in members; aggregate attempt cost remains deduplicated",
                  lineage:"change_id when present; otherwise submissions after the previous merge on the branch"}},
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
        suite:{tests:($phases|map(.scheduled_tests)|distribution),
               scheduled:($phases|map(.scheduled_tests)|distribution),
               executed:($phases|map(.executed_tests)|distribution),
               passed:($phases|map(.passed_tests)|distribution),
               failed:($phases|map(.failed_tests)|distribution),
               skipped:($phases|map(.skipped_tests)|distribution),
               binaries:($phases|map(.binaries)|distribution)},
        selector_comparison:{
          dtest:(($phases|map(select(.selector=="dtest"))) as $s
            | {samples:($s|length), gate_s:($s|map(.elapsed_s)|distribution),
               compile_s:($s|map(.compile_s)|distribution),
               execution_s:($s|map(.execution_s)|distribution),
               scheduled_tests:($s|map(.scheduled_tests)|distribution),
               executed_tests:($s|map(.executed_tests)|distribution),
               skipped_tests:($s|map(.skipped_tests)|distribution),
               binaries:($s|map(.binaries)|distribution)}),
          legacy:(($phases|map(select(.selector!="dtest"))) as $s
            | {samples:($s|length), gate_s:($s|map(.elapsed_s)|distribution),
               compile_s:($s|map(.compile_s)|distribution),
               execution_s:($s|map(.execution_s)|distribution),
               scheduled_tests:($s|map(.scheduled_tests)|distribution),
               executed_tests:($s|map(.executed_tests)|distribution),
               skipped_tests:($s|map(.skipped_tests)|distribution),
               binaries:($s|map(.binaries)|distribution)}),
          stratified:(
            [$cohort_phases
             | group_by([.executed_test_band,.binary_band,.load_band])[] as $cell
             | ([ $cell[] | select(.selector=="dtest") ]) as $d
             | ([ $cell[] | select(.selector!="dtest") ]) as $l
             | {executed_test_band:$cell[0].executed_test_band,
                binary_band:$cell[0].binary_band,
                load_band:$cell[0].load_band,
                matched:(($d|length)>0 and ($l|length)>0
                         and $cell[0].executed_test_band!="unknown"
                         and $cell[0].binary_band!="unknown"
                         and $cell[0].load_band!="unknown"),
                dtest:($d|gate_profile), legacy:($l|gate_profile),
                gate_p50_delta_s:(if ($d|length)>0 and ($l|length)>0
                                  then delta_if_numeric(
                                      ($d|map(.elapsed_s)|distribution|.p50);
                                      ($l|map(.elapsed_s)|distribution|.p50))
                                  else null end),
                execution_p50_delta_s:(if ($d|length)>0 and ($l|length)>0
                                       then delta_if_numeric(
                                           ($d|map(.execution_s)|distribution|.p50);
                                           ($l|map(.execution_s)|distribution|.p50))
                                       else null end)}]
            | {dimensions:{executed_tests:"<100, 100-299, 300-599, 600+",
                           binaries:"1-3, 4-11, 12+",
                           load_per_core:"<1, 1-3, >=3 (or unknown)"},
               matched_cells:(map(select(.matched))|length), cells:.})},
        dtest:(($attempts|map(select(.dtest_status != null))) as $d
          | {samples:($d|length),
             selected_gates:($d|map(select(.test_selector=="dtest"))|length),
             status:($d|group_by(.dtest_status)
                       | map({status:.[0].dtest_status, n:length}) | sort_by(-.n)),
             selection_ms:($d|map(numeric(.dtest_elapsed_ms))|distribution),
             internal_index_ms:($d|map(numeric(.dtest_index_ms))|distribution),
             index_load_ms:($d|map(numeric(.dtest_index_load_ms))|distribution),
             internal_git_ms:($d|map(numeric(.dtest_index_git_ms))|distribution),
             index_stat_ms:($d|map(numeric(.dtest_index_stat_ms))|distribution),
             index_read_ms:($d|map(numeric(.dtest_index_read_ms))|distribution),
             index_parse_ms:($d|map(numeric(.dtest_index_parse_ms))|distribution),
             index_materialize_ms:($d|map(numeric(.dtest_index_materialize_ms))|distribution),
             internal_graph_ms:($d|map(numeric(.dtest_index_graph_ms))|distribution),
             index_serialize_ms:($d|map(numeric(.dtest_index_serialize_ms))|distribution),
             index_map_ms:($d|map(numeric(.dtest_index_map_ms))|distribution),
             internal_selection_ms:($d|map(numeric(.dtest_selection_ms))|distribution),
             selection_git_diff_ms:($d|map(numeric(.dtest_selection_git_diff_ms))|distribution),
             selection_old_sources_ms:($d|map(numeric(.dtest_selection_old_sources_ms))|distribution),
             selection_old_parse_us:($d|map(numeric(.dtest_selection_old_parse_us))|distribution),
             selection_definition_compare_us:($d|map(numeric(.dtest_selection_definition_compare_us))|distribution),
             selection_old_cache_hits:($d|map(numeric(.dtest_selection_old_cache_hits))|add//0),
             selection_old_cache_misses:($d|map(numeric(.dtest_selection_old_cache_misses))|add//0),
             selection_old_cache_lookup_us:($d|map(numeric(.dtest_selection_old_cache_lookup_us))|distribution),
             selection_old_cache_write_ms:($d|map(numeric(.dtest_selection_old_cache_write_ms))|distribution),
             selection_graph_walk_ms:($d|map(numeric(.dtest_selection_graph_walk_ms))|distribution),
             selection_materialize_ms:($d|map(numeric(.dtest_selection_materialize_ms))|distribution),
             selected_tests:($d|map(numeric(.dtest_selected_tests))|distribution),
             executed_tests:($d|map(numeric(.dtest_executed_tests))|distribution),
             selected_not_observed_tests:($d|map(numeric(.dtest_selected_not_observed_tests))|distribution),
             partitions:($d|map(numeric(.dtest_partitions))|distribution),
             overlay_hits:($d|map(numeric(.dtest_overlay_hits))|add//0),
             overlay_entries:($d|map(numeric(.dtest_overlay_entries))|distribution),
             overlay_compacted:($d|map(numeric(.dtest_overlay_compacted))|add//0),
             index_overlay_entries:($d|map(numeric(.dtest_index_overlay_entries))|distribution),
             index_overlay_compacted:($d|map(numeric(.dtest_index_overlay_compacted))|add//0),
             target_narrowed:($d|map(select(.dtest_target_narrowed=="true"))|length),
             exact_filter_fallbacks:($d|map(select(.dtest_exact_filter=="false"))|length),
             filter_bytes:($d|map(numeric(.dtest_filter_bytes))|distribution),
             required_filter_bytes:($d|map(numeric(.dtest_required_filter_bytes))|distribution),
             commands:($d|map(numeric(.dtest_commands))|distribution),
             changed_files:($d|map(numeric(.dtest_changed_files))|distribution),
             changed_definitions:($d|map(numeric(.dtest_changed_definitions))|distribution),
             gate_s:{dtest:($d|map(select(.test_selector=="dtest")|numeric(.gate_elapsed_s // .elapsed_s))|distribution),
                     legacy:($d|map(select(.test_selector!="dtest")|numeric(.gate_elapsed_s // .elapsed_s))|distribution)}}),
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
          | ($subs | map(select(
              (.presubmit_eligible //
               (if ((.touches//"")=="docs") then "false" else "true" end)) == "true"
            ))) as $eligible
          | {submissions:($subs|length),
             presubmit_eligible_submissions:($eligible|length),
             presubmit_ineligible_submissions:(($subs|length)-($eligible|length)),
             actor_adoption:(if ($subs|length)==0 then null
                             else (($subs|map(select((.actor//"unset")!="unset"))|length)/($subs|length)) end),
             trailer_adoption:(if ($subs|length)==0 then null
                               else (($subs|map(select(.trailer_present=="true"))|length)/($subs|length)) end),
             presubmit_receipt_rate:(if ($eligible|length)==0 then null
                                     else (($eligible|map(select(.presubmit_sha_match=="true"))|length)/($eligible|length)) end),
             presubmit_receipt_rate_all:(if ($subs|length)==0 then null
                                         else (($subs|map(select(.presubmit_sha_match=="true"))|length)/($subs|length)) end),
             presubmit_edit_s:($subs|map(numeric(.presubmit_edit_elapsed_s))|distribution),
             missing_receipts_by_touches:($eligible
               | map(select(.presubmit_sha_match!="true"))
               | group_by(if ((.touches//"")|length)==0 then "unclassified" else .touches end)
               | map({touches:(if ((.[0].touches//"")|length)==0 then "unclassified" else .[0].touches end),
                      submissions:length})
               | sort_by(-.submissions, .touches)),
             missing_receipts_by_actor_hint_and_touches:($eligible
               | map(select(.presubmit_sha_match!="true"))
               | group_by([(.actor_hint//"unknown"),
                          (if ((.touches//"")|length)==0 then "unclassified" else .touches end)] | join("|"))
               | map({actor_hint:(.[0].actor_hint//"unknown"),
                     touches:(if ((.[0].touches//"")|length)==0 then "unclassified" else .[0].touches end),
                     submissions:length})
               | sort_by(-.submissions, .actor_hint, .touches)),
             by_actor:($subs|group_by(.actor//"unset")
                       |map({actor:(.[0].actor//"unset"), submissions:length})|sort_by(-.submissions)),
             by_actor_hint:($subs|group_by(.actor_hint//"unknown")
                            |map({actor_hint:(.[0].actor_hint//"unknown"), submissions:length})|sort_by(-.submissions)),
             receipt_rate_by_actor_hint:($subs|group_by(.actor_hint//"unknown")
                                        |map((. as $group
                                             | {actor_hint:($group[0].actor_hint//"unknown"),
                                                submissions:($group|length),
                                                eligible:($group|map(select((.presubmit_eligible // (if ((.touches//"") == "docs") then "false" else "true" end)) == "true"))|length),
                                                receipts:($group|map(select(.presubmit_sha_match=="true"))|length)})
                                             | . + {receipt_rate:(if .eligible==0 then null else (.receipts/.eligible) end)})
                                        | sort_by(-.submissions, .actor_hint)),
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
        work_vs_idle_s:{time_to_land:($landed_rows|map(.total)|distribution),
                        attempt_work:($landed_rows|map(.attempt)|distribution),
                        idle_in_queue:($landed_rows|map(.scheduling)|distribution)},
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
    "Development latency from journal boundaries (p50 / p90 / max)",
    "  final submit to land (n=\(.development_latency.final_submit_to_land_s.count)): \(value(.development_latency.final_submit_to_land_s.p50; "s")) / \(value(.development_latency.final_submit_to_land_s.p90; "s")) / \(value(.development_latency.final_submit_to_land_s.max; "s"))",
    "  scheduling/idle (n=\(.development_latency.scheduling_wait_s.count)): \(value(.development_latency.scheduling_wait_s.p50; "s")) / \(value(.development_latency.scheduling_wait_s.p90; "s")) / \(value(.development_latency.scheduling_wait_s.max; "s"))",
    "  terminal events / unique attempts / duplicate batch members: \(.development_latency.shared_batch_dedup.terminal_events) / \(.development_latency.shared_batch_dedup.unique_attempts) / \(.development_latency.shared_batch_dedup.duplicate_member_events)",
    "  landed changes with failure-driven rework: \(.development_latency.rework.affected_changes)/\(.development_latency.rework.landed_changes)",
    "  failed attempt work / lock wait: \(value(.development_latency.rework.unique_failed_work_s_total; "s")) / \(value(.development_latency.rework.unique_failed_lock_wait_s_total; "s"))",
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
    "dtest affected-test selection",
    "  selected gates / samples: \(.dtest.selected_gates) / \(.dtest.samples)",
    "  selection ms p50 / p90:   \(value(.dtest.selection_ms.p50; "ms")) / \(value(.dtest.selection_ms.p90; "ms"))",
    "  index/git/graph p50:       \(value(.dtest.internal_index_ms.p50; "ms")) / \(value(.dtest.internal_git_ms.p50; "ms")) / \(value(.dtest.internal_graph_ms.p50; "ms"))",
    "  index stat/read/parse p50: \(value(.dtest.index_stat_ms.p50; "ms")) / \(value(.dtest.index_read_ms.p50; "ms")) / \(value(.dtest.index_parse_ms.p50; "ms"))",
    "  index copy/write/map p50:  \(value(.dtest.index_materialize_ms.p50; "ms")) / \(value(.dtest.index_serialize_ms.p50; "ms")) / \(value(.dtest.index_map_ms.p50; "ms"))",
    "  selection p50 / p90:       \(value(.dtest.internal_selection_ms.p50; "ms")) / \(value(.dtest.internal_selection_ms.p90; "ms"))",
    "  old-source Git/parse p50:  \(value(.dtest.selection_git_diff_ms.p50; "ms")) / \(value(.dtest.selection_old_sources_ms.p50; "ms")) / \(value(.dtest.selection_old_parse_us.p50; "us"))",
    "  graph/materialize p50:     \(value(.dtest.selection_graph_walk_ms.p50; "ms")) / \(value(.dtest.selection_materialize_ms.p50; "ms"))",
    "  selected tests p50 / p90: \(value(.dtest.selected_tests.p50; "")) / \(value(.dtest.selected_tests.p90; ""))",
    "  actually ran p50 / p90:   \(value(.dtest.executed_tests.p50; "")) / \(value(.dtest.executed_tests.p90; ""))",
    "  selected not observed:    \(value(.dtest.selected_not_observed_tests.p50; "")) p50",
    "  target-narrowed gates:     \(.dtest.target_narrowed)",
    "  exact-filter fallbacks:    \(.dtest.exact_filter_fallbacks)",
    "  filter bytes p50 / p90:    \(value(.dtest.filter_bytes.p50; "")) / \(value(.dtest.filter_bytes.p90; ""))",
    "  required bytes p50 / p90:  \(value(.dtest.required_filter_bytes.p50; "")) / \(value(.dtest.required_filter_bytes.p90; ""))",
    "  overlay hits / compacted:  \(.dtest.overlay_hits) / \(.dtest.overlay_compacted)",
    "  graph-stable index overlay: \(value(.dtest.index_overlay_entries.p50; "")) entries p50; \(.dtest.index_overlay_compacted) compacted",
    "  dtest / legacy gate p50:  \(value(.dtest.gate_s.dtest.p50; "s")) / \(value(.dtest.gate_s.legacy.p50; "s"))",
    ("  outcomes:                 " + (.dtest.status | map("\(.status)=\(.n)") | join(", "))),
    "",
    "dtest versus legacy execution",
    "  samples:                   \(.selector_comparison.dtest.samples) / \(.selector_comparison.legacy.samples)",
    "  gate p50:                  \(value(.selector_comparison.dtest.gate_s.p50; "s")) / \(value(.selector_comparison.legacy.gate_s.p50; "s"))",
    "  compile p50:               \(value(.selector_comparison.dtest.compile_s.p50; "s")) / \(value(.selector_comparison.legacy.compile_s.p50; "s"))",
    "  execution p50:             \(value(.selector_comparison.dtest.execution_s.p50; "s")) / \(value(.selector_comparison.legacy.execution_s.p50; "s"))",
    "  executed tests p50:        \(value(.selector_comparison.dtest.executed_tests.p50; "")) / \(value(.selector_comparison.legacy.executed_tests.p50; ""))",
    "  skipped tests p50:         \(value(.selector_comparison.dtest.skipped_tests.p50; "")) / \(value(.selector_comparison.legacy.skipped_tests.p50; ""))",
    "",
    "Matched workload cohorts",
    "  matched cells:             \(.selector_comparison.stratified.matched_cells)",
    (if .selector_comparison.stratified.matched_cells == 0 then
       "  no cohort has both selectors with known workload and load bands yet"
     else
        (.selector_comparison.stratified.cells
        | map(select(.matched)
              | "  tests=\(.executed_test_band), binaries=\(.binary_band), load/core=\(.load_band): n=\(.dtest.samples)/\(.legacy.samples), gate p50=\(value(.dtest.gate_s.p50; "s"))/\(value(.legacy.gate_s.p50; "s")) (delta \(value(.gate_p50_delta_s; "s"))), execution p50=\(value(.dtest.execution_s.p50; "s"))/\(value(.legacy.execution_s.p50; "s")) (delta \(value(.execution_p50_delta_s; "s")))")
        | join("\n"))
     end),
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
    "  green shard receipt:     \(if .attribution.presubmit_receipt_rate==null then "n/a" else ((.attribution.presubmit_receipt_rate*100|round|tostring)+"%") end) (\(.attribution.presubmit_eligible_submissions) eligible; \(.attribution.presubmit_ineligible_submissions) no focused shard)",
    (if (.attribution.missing_receipts_by_touches|length)==0 then
        "  missing receipt surfaces: none"
     else
        ("  missing receipt surfaces: "
         + (.attribution.missing_receipts_by_touches
            | map("\(.touches)=\(.submissions)") | join(", ")))
     end),
    (if (.attribution.missing_receipts_by_actor_hint_and_touches|length)==0 then
        "  missing receipt cohorts: none"
     else
        ("  missing receipt cohorts: "
         + (.attribution.missing_receipts_by_actor_hint_and_touches
            | map("\(.actor_hint)/\(.touches)=\(.submissions)") | join(", ")))
     end),
    "  edit check p50 / p90:    \(value(.attribution.presubmit_edit_s.p50; "s")) / \(value(.attribution.presubmit_edit_s.p90; "s")) (\(.attribution.presubmit_edit_s.count) samples)",
    "  actor set (not \"unset\"): \(if .attribution.actor_adoption==null then "n/a" else ((.attribution.actor_adoption*100|round|tostring)+"%") end)",
    ("  actor hints:              " + (.attribution.by_actor_hint | map("\(.actor_hint)=\(.submissions)") | join(", "))),
    ("  receipts by actor hint:   " + (.attribution.receipt_rate_by_actor_hint | map("\(.actor_hint)=\(.receipts)/\(.eligible)") | join(", "))),
    "  Co-Authored-By trailer:  \(if .attribution.trailer_adoption==null then "n/a" else ((.attribution.trailer_adoption*100|round|tostring)+"%") end)",
    (if (.red_causes|length)==0 then "" else
        ("\nFailed gate time by cause\n"
         + (.red_causes | map("  \(.red_class): n=\(.n)  \(value(.gate_minutes; "m"))") | join("\n")))
     end),
    "",
    "Discovery is estimated as test-stage wall time minus Cargo compile and nextest execution.",
    "First-edit latency is unavailable: the journal begins at submit/resubmit.",
    "Scheduling/idle can include coordinator unavailability or machine sleep; the journal cannot distinguish them.",
    "A gate is only comparable to another at similar load per core; see the contention buckets.",
    "A green shard receipt certifies the AUTHOR'"'"'S tree, not the rebased tree the gate runs."
' "$report"

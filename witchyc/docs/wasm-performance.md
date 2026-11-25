# Direct Wasm performance evidence

The retained SDK read-ahead and narrow Lean imports improve a bounded proof-bearing Vec client while retaining complete content checks and every source-bound obligation. These are optimized same-host observations; the compiler, VC translation and physical runtime remain trusted.

| Matched edit comparison (12 pairs each) | Paired candidate/baseline ratio | 95% bootstrap interval |
| --- | --- | --- |
| Read-ahead, Vec runtime-code edit |0.72256|0.69445–0.72741|
| Narrow imports after read-ahead, Vec runtime-code edit |0.87634|0.85875–0.90587|
| Narrow imports after read-ahead, authored Vec proof edit |0.91501|0.85579–1.03984 (unresolved)|
| Direct original → both changes, authored Vec proof edit |0.63736|0.60588–0.67811|

The direct combined proof-edit result is **36.3% lower paired elapsed time**, with all 12 pairs faster. Baseline/final medians were 6.6847/4.3563 s; paired median difference was −2.4584 s (interval −2.8817 to −2.1437). The authored edit strengthens a real postcondition and preserves runtime bytes/output. Do not multiply intervals from the separate experiments. Paired proof-free controls showed no resolved change.

Full installation A+B snapshots, ordered content coverage, compiler hashes, fresh kernel/trust0, expected types, closed theorem/axiom audit, currentness and atomic publication remain. The read-ahead identity is still SHA256 v1; no cache, immutable-store claim, SDK narrowing or admission reuse is involved. Whole Lean-child time includes startup/imports/elaboration/tactics/kernel/audit/scheduling; it is not kernel-only.

The direct comparison's nested campaign passed all 24 rows and guards. Its outer driver then read the preceding 72-row campaign by mistake and failed a 24-row assertion. That FAILED record is preserved. Independent review checked actual nested rows, pair coverage, fingerprints/bindings, statistics and outer source/pilot/reap guards; a separate adjudication qualifies the measurement without relabeling the failed driver or rerunning it.

## Final-candidate scaling

Nine cases × fresh/unchanged/edit × three samples passed: 81 timed checks plus 73 separate coverage/warmup/setup checks, all with correct consumed runtime output. Campaign elapsed 438.416 s; timed compiler calls totalled 224.056 s, setup compiler calls 200.083 s, and separate host calls 13.171 s. Setup is excluded from the table. Fresh means absent output, not cold filesystem. Unchanged and edits recheck all proofs, with no cache hit or admission reuse.

| Case | Lines / bytes | Selected functions / emitted instances / goals | Fresh | Unchanged | Edit |
| --- | --- | --- | --- | --- | --- |
|chain-16|36 / 1132|17 / 17 / 0|0.0160 (0.0142–0.0160)|0.0191 (0.0136–0.0221)|0.0135 (0.0127–0.0240)|
|chain-64|132 / 4300|65 / 65 / 0|0.0155 (0.0142–0.0181)|0.0187 (0.0138–0.0206)|0.0161 (0.0139–0.0166)|
|chain-120|244 / 8036|121 / 121 / 0|0.0195 (0.0170–0.0279)|0.0192 (0.0136–0.0200)|0.0153 (0.0148–0.0179)|
|ghost-1|6 / 126|1 / 1 / 1|3.6435 (3.6294–3.6709)|3.7299 (3.6025–3.9285)|3.6354 (3.5290–3.8717)|
|ghost-8|34 / 595|1 / 1 / 8|3.6954 (3.6811–3.7671)|4.3465 (3.6988–5.3468)|3.6426 (3.5688–3.8244)|
|ghost-32|130 / 2225|1 / 1 / 32|3.7963 (3.7556–3.8319)|3.8021 (3.6750–4.0092)|3.8065 (3.6861–3.8945)|
|vec|106 / 4289|9 / 21 / 2|4.2299 (4.1344–6.6610)|5.1762 (4.9196–5.7958)|4.1286 (4.1172–4.6465)|
|ghost-1-proof|6 / 126|1 / 1 / 1|3.6878 (3.5787–3.9301)|3.7411 (3.6284–4.5053)|3.8000 (3.7549–4.0120)|
|vec-proof|106 / 4289|9 / 21 / 2|4.1592 (4.0846–4.1905)|4.3434 (4.0626–4.8522)|6.0688 (4.2212–6.0920)|

Times are seconds, median (min–max), n=3. This descriptive scaling pilot establishes no new speedup or confidence interval. Variability is visible: Vec authored-proof edits ranged 4.2212–6.0920 s, and ghost8 unchanged 3.6988–5.3468 s. These do not overturn the separate 12-pair results or establish a state-specific regression.

Chain cases have one generic Box type and call every selected function; 36/132/244 lines separate ordinary code size from proof count. Ghost cases contain 1/8/32 independent tiny lemmas and emit only main; they do not measure runtime code growth or increasingly complex proofs. Vec is the actual fixed two-goal generic push-spare/caller exercised at four layouts, not a full Vec growth/loop proof or a Vec-size sweep. Its 9 source functions become 21 instances. Proof-edit cases keep runtime fixed while changing verified statements; Vec edited source has 107 lines. Other edits change consumed runtime output.

Code-edit phase medians: installation A+B were 2.8019/2.8082/2.8081 s for ghost1/8/32, with whole child 0.7948/0.8161/0.9603 s. Vec installation A+B was 2.7940 s and child 1.3152 s. Chain16/64/120 static-check medians were 0.1205/0.2732/0.4208 ms with zero Lean invocations and zero installation snapshots. Authored-proof rows use receipt mode and do not expose phases; no phase times are invented for them.

The bounded direct-Wasm profile caps source/module at 64 KiB and host functions at 256 including helpers/imports; larger 1k/10k-line workloads cannot be claimed from this corpus. Lean remains 10 s/2048 MiB/64 KiB output. All selected claims, byte erasure, false-unused refusal, artifact tampering and publication safeguards passed focused correctness qualification before timing. This is not the old native/full-Vec/797-VC workload, a compiler correctness proof, cold-cache/RSS/hardware-counter result, dedicated-host measurement or universal optimality claim.

Pinned final compiler: c227765690d714ba41872180954370766899197b, SHA256 `309048e4c38f70f9e004bf7c0d00cc2328ff614eaaadd06c271ac87e0bc6043d`; harness/source snapshot14e88651f4d24fd878d7337707399aaa23460ba4. Host macOS-27.2-arm64-arm-64bit-Mach-O, Node v22.23.4. All source/compiler/fixture/pilot/reaping guards passed, with at least 33.19 GiB free (16 GiB launch/4 GiB live floor). No SDK installation/selection or privileged action occurred.

Raw evidence is outside source control under generic/witchy-wasm-coordinator-94: perf-paired-783, perf-imports-paired-794, perf-combined-proof-795 and its separate driver/adjudication-797.json, perf-final-scaling-798 and its driver. Independent review: coordination results/WASM-PERF-EVIDENCE-REVIEW-797-r1.md. Exact commands, binary/harness/fixture pins, individual rows, receipts and logs remain there.

The [versioned BLAKE3 trial](wasm-blake3-trial.md) passed focused correctness but regressed matched SDK hashing and proof-bearing edits, so SHA256 remains qualified. The protected SDK action remains pending.

## Additional bounded experiments

The separate SDK tree-digest prototype hashes every file in four bounded reader workers and folds ordered file digests, with versioned identities and both complete installation snapshots. Focused correctness and artifact controls passed. Its three-pair pilot showed no material benefit against qualified SHA256: code-edit ratio 1.01556 (mixed pairs), authored-proof-edit ratio 1.07156 (all three slower). These are descriptive observations without a confidence interval; no confirmation campaign or integration was justified. This did not compare the tree scheme directly with the earlier raw BLAKE3 scheme. The candidate and evidence remain held separately.

A separate public-frontend resident diagnostic also remains unqualified. The same captured Vec proof that passed a fresh CLI check refused in the interpreted helper under the unchanged 2048 MiB budget, even after matching the CLI's inner memory option. The maximum sampled helper RSS was 2,254,224 KiB; this is sampled RSS, not a measured allocator peak or established root cause. Kernel memory errors led to an audit refusal for `sorryAx`; no proof was admitted. Cleanup reported EPERM and retained an unknown group status. A subsequent independent process census found the recorded helper and group absent, permitting resource release without treating EPERM as evidence of exit. No memory increase, counter reset, proof-check weakening or performance claim followed.

The resident attempt is frozen rather than an eligible compiler optimization. Fresh CLI checking remains authoritative. Raw evidence: `sdk-tree-pilot-827/results.json`, its driver, `resident-option-match-830-b/{results.json,probe-output/results.json}`, and the separate fresh CLI reference `resident-fresh-reference-828`, under the same generic evidence root. Neither unsuccessful experiment changes the retained measurements above or establishes that further structural improvements are impossible.

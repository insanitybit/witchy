# General async matrix

`python3 scripts/async-general-bench.py --generate-only --messages 1024 --output NEWDIR` emits paired source files and independently computed expected results. Counts must be a positive multiple of16. Measurement uses `--baseline`, `--candidate`, `--baseline-std`, `--candidate-std`, optional `--go`, and a new `--output` directory. Repeat `--workload` to select rows. Default:12 balanced measured repetitions after2 warmups; compiler binary/source hashes, exact outputs, cache state, process wall/RSS, and paired bootstrap intervals are retained.

| Rows | Operation and correctness |
| --- | --- |
| packet2 | Current closed-region control, original send/fold shape |
| packet3, packet8 | Width-only nominal Int aggregates; independent field-sum checksum |
| helper_packet3 | Same3-field packet, with an awaited send helper |
| packet_bool, packet_string | Mixed payloads; FIFO/payload validity changes checksum to -1 on mismatch |
| dynamic_4x1, dynamic_16x1 | Runtime-built Task lists and fan-in; per-producer FIFO and sum |
| dynamic_4x4 | Runtime-built Task lists and many-to-many; per-consumer subsequence FIFO and total sum |

Dynamic rows prepare channels and lazy jobs before the kernel clock; Go prepares closures before timing and starts no goroutines until kernel entry. Both kernels include task activation, completion, and result collection. Fixed packet rows retain the existing inclusive kernel boundary, including channel/task setup; their setup time is not reported separately. Compilation/admission/JIT live outside the in-program kernel and remain in process wall. These are CPU/channel workloads. A source shape is a coverage description; actual direct-path selection requires separate compiler evidence.

Cancellation is supported by the existing chan.race contract and remains a pending matched CPU row. Timer waits are not claimed: canonical Clock registration in crates/witchy-runtime/src/runtime/host/clock_env_rand.rs exposes clock_now and clock_now_monotonic, and task Step has no timer-wait operation. The illustrative future.select sleep comment is not an implementation. Real asynchronous I/O is deferred: filesystem registration in runtime/host/filesystem.rs uses synchronous func_wrap callbacks; CPU channel measurements do not establish suspendable I/O performance.

Run campaigns only in a coordinated serialized benchmark window. Source/compiler smoke validation is distinct from a performance campaign. No RSS value or old exported zero counter is treated as guest allocation evidence.

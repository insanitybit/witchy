# Installation digest read-ahead

Focused correctness and matched optimized measurements qualified this change at
633cb03. Twelve interleaved consumed Vec-edit pairs on one Darwin arm64 host
reduced median elapsed time from 6.4681 s to 4.6595 s. The paired median ratio was
0.7226 (95% bootstrap interval 0.6945–0.7274), or 27.7% lower time for that workload.
Complete installation snapshots A+B fell from median 4.6371 s to 2.7722 s; whole
Lean-child time stayed about 1.85 s. The proof-free 16-function control showed no
resolved change (paired difference interval −2.77 to +2.86 ms).

This is a finite-sample warm-session result for the actual four-layout caller,
not a cold-filesystem, general-program, multicompiler-throughput or kernel-only
claim. All source/compiler/fixture/process guards passed. Both full snapshots,
the exact v1 identity and every source-bound proof obligation remain required.

`installation(root)` still computes the exact `witchy.scalar.lean-installation.v1` SHA256 stream: sorted depth-first relative names, original length framing, directory/file/symlink tags, little-endian file lengths, every content byte, link text and resolved target names. One main-thread CommonCrypto context consumes events in that exact order. Compiler-file hashing, algorithm/version/schema, proof A/B full snapshots, SDK selection, grants, proof/audit/publication policy and the zero-obligation path are unchanged. There is no cache, manifest trust, Merkle identity or reuse.

Four fixed readers perform regular-file open/read ahead. Each has at most one active job; its per-file chunk channel holds at most four 64 KiB chunks and the reader may hold one additional chunk. A reader is reassigned only after its ordered end marker is consumed; files close before that marker is sent. Across four readers plus one consumer chunk, content storage is at most 21 chunks (1,376,256 bytes), with at most 64 KiB queued metadata and 64 ordered events, plus small bounded channel/job/path state and the existing bounded sorted directory traversal. Oversized metadata is consumed directly after preceding events drain. At most four regular content files are open; there is no unbounded manifest, file-content allocation, thread-per-file or persistent descriptor.

All existing entry/depth/size/UTF8/symlink/root/special-file guards remain in the same walker. Readers consume the declared length and require EOF afterward, retaining truncation and growth checks. The existing immutable-during-check assumption remains: lookahead can change the timing/order of observations but never authorizes a digest from a failed read or invalid tree. Every normal return/error and Rust unwind drops queued reply receivers, closes job channels and joins every owned reader; read or worker failure prevents a digest. Completed readers are joined before returning an identity, hence before Lean or the second identity phase. Process abort/termination retains ordinary process-level semantics.

Executed focused tests compare with an independent cfg(test) serial v1 walk on
empty/mixed/nested/multi-chunk/symlink fixtures with twelve regular files, identity
mutations, invalid links/socket/depth, changed lengths, repeated queued errors,
late walker failure and unwind cleanup. The real installation identity, expected
obligations, model/audit/generated module and Wasm exactly matched baseline; real
four-layout execution and false-unused-proof publication preservation passed.
APFS rejects non-UTF8 fixture creation itself, so that compiler guard was source
reviewed here; worker panic/partial-spawn and 100,000-entry/8-GiB boundaries were
not injected. Actual RSS/allocation counters were not measured; the data bound
above excludes stacks and existing traversal metadata. No SDK installation,
selection, cache/admission shortcut or privileged action is part of this change.

# RFC-0165 launch arguments independent review (read-only)

Scope was limited to the named entry-metadata, launch writer, runtime insertion, interpreter oracle, and focused probe files. No build was run.

## Verdict

No concrete authorization, metadata, RC-image, extent, or allocator-frontier bypass was found in this bounded review.

## Checked boundaries

- **Source/ABI authorization:** `derive_entry` requires the validated parameter to be precisely the catalog `List(String)` layout *and* the checked source parameter to be syntactically `List(String)`. It rejects every other reference parameter before entry metadata is minted (`crates/witchy-wir/src/canonical_wasmir.rs:6781-6835`). The fixed catalog identities pin the string as a four-byte-length byte buffer and the list as an RC, `(length, capacity, owned-pointer-lane)` layout (`crates/witchy-wir/src/canonical_launch.rs:9-30`).
- **Metadata indices:** v2 decoding rejects missing position bytes, zero argument count, duplicate/non-increasing positions, positions outside the complete argument count, and trailing bytes; v1 rejects trailing bytes (`crates/witchy-wir/src/canonical_wasmir.rs:1521-1596`). Runtime starts from the ordered capability list and inserts each independently owned `List(String)` pointer at the validated absolute position (`crates/witchy-runtime/src/runtime.rs:1456-1462`).
- **Host image:** the writer uses checked arithmetic for list size, aggregate allocation size, every string allocation, and `base + total`; it grows memory before the write and advances the exported bump frontier exactly to the end. The image lays out RC count/payload-size prefixes, list count/capacity, eight-byte owned pointer lanes, and string length/bytes consistently with the pinned catalog (`crates/witchy-runtime/src/runtime/launch_arguments.rs:12-77`). It leaves the free-list head untouched. A launch image is built only after capability validation (`crates/witchy-runtime/src/runtime.rs:1428-1461`).
- **Oracle separation:** the interpreter builds objects/cells rather than host bytes or encoder instructions, preserves one owned string token per list lane, and computes leaks from interpreter allocation state (`crates/witchy-wir/src/canonical_interpreter/launch.rs:1-64`). The probes compare that semantic path against admitted Wasm output and the runtime heap audit (`tests/misc/rfc0165_launch_arguments.rs:1-96`).

## Bounded residuals, not findings

- The oracle shares the fixed `argument_layouts()` catalog constructor with admission and the host writer. That is an intentional identity contract rather than byte-writer self-attestation: its object construction does not consume the writer's bytes.
- This review does not claim safety for external raw Wasm or unreviewed runtime entry paths. It assesses the named admitted-artifact launch path only.

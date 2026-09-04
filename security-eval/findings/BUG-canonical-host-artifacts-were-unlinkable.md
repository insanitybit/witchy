# Canonical host artifacts were unlinkable

## Impact

Canonical HostCall encoding discarded the authenticated physical ABI linkage
and emitted placeholder names such as `witchy::canonical.import.0`. It also did
not export guest linear memory. A real runtime could therefore neither bind the
declared host function nor read an authenticated guest buffer. Admission still
accepted the internally self-consistent artifact, but execution could not link.

## Fix

`ImportContract` now carries the physical module and field name projected from
the generated ABI catalog. Those names are included in the immutable module
digest and grant no authority themselves; checker equality against the
authenticated catalog supplies authority. The backend and independent metadata
plan emit the checked linkage, and canonical artifacts export memory for host
buffer access.

`console_print_reaches_admitted_execution` binds `witchy::print`, reads the
checked UTF-8 slice from exported guest memory, and observes exactly `b"ok"`.

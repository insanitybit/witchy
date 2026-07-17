# C4: The Continuously Concurrent Compacting Collector

- **Authors / venue:** Gil Tene, Balaji Iyengar, Michael Wolf (Azul Systems). ISMM 2011. DOI 10.1145/1993478.1993491.
- **PDF:** **Not stored** — gated behind ACM (direct fetch returns HTTP 403). Catalog-only entry; retrieve manually from the ACM DL or an authenticated mirror if needed.
- **What it is:** A generational, **load-barrier**-based, continuously-concurrent **compacting** collector (the productionized form of Azul's Pauseless GC). The read barrier supports concurrent compaction, remapping, and incremental-update tracing, so pause times are decoupled from heap/live-set size — the intellectual ancestor of OpenJDK **ZGC**'s colored-pointer + load-barrier design.

## Why it matters to witchy

The reference point for the **tracing-GC path witchy is choosing *not* to take.** The ZGC/Green Tea discussion (user-raised) is this lineage: barriers buy concurrent relocation on large, long-lived, *cyclic, shared-mutable* heaps. witchy has none of those properties — value semantics make the heap acyclic and unshared — so RC (see the [Perceus notes](../perceus-2021/notes.md)) fits better and avoids barriers/relocation machinery entirely. Keep C4 as the "why not tracing" counterweight in the design record.

## Informs

- The comparison section of the memory-management design discussion (vs. the Go Green Tea blog and the OpenJDK ZGC wiki).

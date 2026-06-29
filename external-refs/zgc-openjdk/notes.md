# ZGC — The Z Garbage Collector (OpenJDK)

- **Source:** OpenJDK ZGC wiki, https://wiki.openjdk.org/spaces/zgc/pages/34668579/Main (user-provided). Principal authors: Per Liden, Stefan Karlsson, et al. (Oracle).
- **Snapshot:** **Not stored** — the wiki Main page is JS-rendered and did not fetch to static markdown. Notes below are from established ZGC documentation/talks; treat the wiki URL as the live source of record.

## What it is

A **concurrent, region-based, compacting (relocating)** collector for the JVM whose headline property is **sub-millisecond max pause times that are decoupled from heap size** — pauses are bounded by the **root-set** size, not the live set, so it scales to multi-TB heaps. Nearly all work (marking, relocation/compaction, remapping) runs **concurrently** with the application.

Mechanism:
- **Colored pointers** — metadata bits embedded in 64-bit references (marked0/marked1/remapped/finalizable) encode each pointer's GC state (historically realized via multi-mapped virtual memory).
- **Load barrier** — a read barrier on reference loads that lazily *heals* (remaps) a pointer to a relocated object on first access, enabling **concurrent compaction** without a stop-the-world relocation pause. (Generational ZGC adds **store barriers**.)
- **Generational ZGC** (JDK 21, 2023) — separate young/old generations: most objects die young → frequent cheap young collections, infrequent old ones. A large throughput/footprint win over single-generation ZGC.

This is the productionized descendant of the Azul Pauseless/C4 lineage (see [[c4-concurrent-compacting-2011]]).

## Why it matters to witchy

The state of the art for low-latency tracing on **large, long-lived, mutable, shared, cyclic** heaps. witchy has **none** of those properties — value semantics make its heap acyclic and unshared — so ZGC's barrier + relocation machinery solves problems witchy doesn't have. The one transferable idea is **generational** ("most objects die young"), which maps cleanly onto witchy's region/scope model (most allocations die at scope exit) and onto RC's eager free. Keep ZGC as the "why RC, not tracing" counterweight.

## Informs

- The GC-comparison section of the memory-management design discussion, alongside [[go-green-tea-gc-2025]] and [[c4-concurrent-compacting-2011]].

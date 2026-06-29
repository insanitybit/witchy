# The Green Tea Garbage Collector (Go blog)

- **Authors / source:** Michael Knyszek, Austin Clements. The Go Blog, 29 Oct 2025. URL: https://go.dev/blog/greenteagc (based on Knyszek's GopherCon 2025 talk). Page snapshot indexed in the context-mode sandbox under source "Go Green Tea GC blog".
- **Status:** Experimental in Go 1.25 (`GOEXPERIMENT=greenteagc`); ~10% less GC time on many workloads, up to **40%** on some; production-use at Google; planned **default in Go 1.26**.

## What it is

A redesign of the **mark** phase of Go's concurrent tricolor mark-sweep GC. The motivating measurements: **~90% of GC cost is marking** (only ~10% sweeping), and **≥35% of marking time is stalled on memory access**. The classic graph-flood (mark objects one at a time by chasing pointers) is a "microarchitectural disaster" — it jumps all over memory doing tiny bits of work, so the CPU can't prefetch or pipeline, and a main-memory miss is ~100× a cache hit.

Green Tea's idea: **"work with pages, not objects."** Scan whole **pages/spans**, keep *pages* (not objects) on the work list, and track marked objects **locally per page**. Because a page holds many same-size objects, scanning becomes contiguous and **SIMD-able**: an **AVX-512 kernel** uses per-page `seen`/`scanned` bitmaps and a pointer/scalar bitmap to scan many objects' words at once. Net effect: spatial locality during marking.

## Why it matters to witchy

A precise statement of the cost witchy's allocator structurally avoids: **bump/region allocation is contiguous**, exactly the locality tracing GCs fight to recover. More importantly, Green Tea is heroics to make tracing's *worst* phase (the mark graph-flood) less cache-hostile — and **witchy's chosen direction (RC on an acyclic value heap) has no mark phase at all.** So this is a counterweight in the "why not tracing" record: if witchy ever *did* add tracing (e.g. a shared long-lived heap), the lesson is span/page-oriented scanning + type bitmaps; absent that, RC sidesteps the entire problem class.

## Informs

- The GC-comparison section of the memory-management design discussion. See [[c4-concurrent-compacting-2011]] (the relocation/barrier side) and [[zgc-openjdk]].

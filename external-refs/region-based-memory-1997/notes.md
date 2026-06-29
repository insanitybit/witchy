# Region-Based Memory Management

- **Authors / venue:** Mads Tofte, Jean-Pierre Talpin. Information and Computation 132(2):109–176, 1997. (PDF: SNU ROPAS mirror.)
- **What it is:** The foundational region paper. The store is a **stack of regions**; every value is allocated into some region; all allocation/deallocation points are **inferred automatically by a type-and-effect system**. Deterministic, no GC, no per-object cost — memory is freed in bulk when a region is popped.

## Why it matters to witchy

The theory under witchy's `region:` blocks and the heap **watermark** reclamation (`$heap` saved on entry, reset on exit). The "smallest scope a value is confined to" question that `rfcs/performance-modes.md` wants to unify into one **escape/region lattice** is precisely region inference. Regions are the **RC-elision tier**: allocations proven confined to a scope skip reference counting entirely and free in bulk.

## Informs

- `rfcs/regions.md`; the escape/region lattice consolidation (`rfcs/performance-modes.md` "NEXT").

## Caution

Pure Tofte–Talpin regions have a well-known failure mode: a value forced into a long-lived region **leaks** until that region dies (no early free). This is exactly why witchy should keep regions as *a tier*, not the whole reclamation story — escaping/long-lived values still need eager-free or RC. See the "A Retrospective on Region-Based Memory Management" (Tofte et al. 2004) for the practical post-mortem.

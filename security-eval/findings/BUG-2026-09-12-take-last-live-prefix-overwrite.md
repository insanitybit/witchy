# TakeLast lost live-prefix overwrite protection

Baseline: 2a18c4a85 (integrated as c5cc41710). Independent review by Noether.

The candidate initializes owning list [a, b], extracts slot 1 with TakeLast,
then stores a new owning c at slot 0. TakeLast clears slot/range facts and
marks the shortened list complete. Store consulted that summary only for
fixed fields, and its element-range lookup compared SSA IDs rather than
allocation identity. Thus the live a could be overwritten without release,
also after MoveValue. Existing value/parity tests did not cover this sequence.
This is a genuine ownership-checker defect, not permission to weaken stores.

The correction keeps a shared-child initialized-prefix range after extraction.
Element Store checks both the complete logical prefix and all live ranges by
allocation identity, requiring proved disjointness. Moved-out ranges are not
live ownership. Unknown overlap refuses. UpdateCount and MoveRange rules are
unchanged. Tests require exact overwrite refusal, and allow append after pop
both directly and through MoveValue, with oracle/native execution.

The same review identified a scalar oracle discrepancy: native allocation is
monotonic into fresh zero-filled linear memory, while scalar TakeLast required
an explicit cell and erased its bytes. Scalar extraction now reads implicit
zero or stored bytes and retains those bytes beyond the shorter logical count.
Reference cells still transfer one owning edge; Unit still requires explicit
initialization. Tests compare no-store scalar zero and republished stored bytes
against real admitted Wasm. This does not relax reference initialization.

The generated-helper test also checks actual raw-source lowering/admission:
normal callers prepare snapshot copies before unique calls; scalar/reference
extraction helpers permit Option allocation but no receiver/payload allocation,
range/byte copies or hidden calls. The existing three-leg cases remain required.

Execution results are reported in the corrective commit handoff; this note
does not claim that the pre-correction counterexample was executed.

## Follow-up: stale alias prefix prevented complete transfer

Independent review of 94f48e940 found a functional refusal after pop, MoveValue,
append and complete MoveRange. Prefix seeding/consolidation looked up exact SSA
tokens, duplicating the consumed alias's old prefix and leaving it behind when
extending the moved token's prefix. The unchanged source-completeness check
then correctly refused MoveRange(source-partial). No unsafe admission was
identified in this follow-up.

The narrow correction locates and replaces the published prefix by allocation
identity and carries its Shared status. It consolidates only the published
edge, leaving unrelated unpublished stores/ranges for the existing checks.
Controls move a list containing a still-shared child through pop, append and
complete transfer, directly and with token moves before/after the append store.

Synthetic zero-start prefixes also no longer match their nonzero SSA placeholder:
zero_start means exactly zero, not either zero or the placeholder's value.

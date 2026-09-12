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

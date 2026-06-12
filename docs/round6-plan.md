# Learner round 5 (ambitious mode) — evaluation, and the round-6 worklist

Round 5 pushed deliberately into heavier territory: 21 programs including a
5-rune diamond-dependency project over 3,000-line input, multi-actor
topologies, deep bounded generics, and large lazy pipelines. 12/21 first-try
(ambition has a cost); zero silent-wrong findings; fmt byte-identical or
behavior-preserving everywhere, including the whole megaproject.

## Evaluation

Scale found the seams that toy programs never touch — both Blockers were
limits, not logic, and both are fixed (5144b10):

1. **The interpreter step budget** capped legitimately-large workloads while
   WASM ran them fine, making `parity` itself diverge. Programs now run
   unbudgeted on both backends (an infinite loop hangs identically); the
   ceiling survives only where termination is contractual — `comptime:`.
   The standing rule it taught: **the reference implementation may not have
   resource ceilings the deployment backend lacks** — any such limit is a
   manufactured divergence.
2. **Actor data fields without defaults** were interpreter-only. A no-init
   `Int` field is now an exported global the host fills from the spawn
   argument (the Subject-id path); each instance gets its own value.

Also landed between rounds: `to_string` is gone (b8a15fe) — the global
namespace is now exactly capability ops + `fail`, and `${...}` is the one
rendering spelling, with fmt as the migration vehicle.

## Round 6 — the friction list

1. **Trait dispatch reach** (the standing constraint, now hit in anger):
   in `where a: Trait` functions, trait calls resolve only on params and
   for-loop vars — not match-pattern bindings, not call results, not through
   bounded helpers. The generic "unknown function" error compounds it. Two
   tiers: (a) better error ("`weight` is a trait method; in a bounded
   generic it resolves only on …"), (b) extend head-typing to match binders
   (ctor_fields already types them for `match` — thread it into the bounded
   scope).
2. **Spawn-supplied String/List/Float fields** — Int works now; the other
   value shapes still demand defaults on WASM only. Either extend the spawn
   path (String via host cells) or make typeck reject uniformly with a
   "give it a default" error on BOTH backends so the gap is symmetric.
3. **Reserved-word hints**: `region`/`sink` as field/param names die with
   bare "expected identifier" — name the collision ("`region` is a reserved
   word").
4. **No-op match arm**: document (or bless) an idiom for "do nothing here" —
   the learner reached for `None -> {}` and got an unhelpful rejection.
   Candidate: accept `-> {}` as an explicit empty statement arm, or teach
   `_ -> 0` discard style in the book's match section.
5. **Actor scheduler message budget** (1M messages) is the same
   manufactured-divergence class as the step budget, one level up — audit
   whether the WASM actor host has an equivalent; align them.

Then learner round 6, same ambitious brief.

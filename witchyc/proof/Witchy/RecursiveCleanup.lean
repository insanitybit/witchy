import Witchy.CleanupWorkStep

namespace Witchy.Storage

/-- Pausing retains work; refusal is distinct from successful completion. -/
inductive CleanupOutcome where
  | complete
  | paused
  | refused (reason : Memory.Error)
  deriving Repr, DecidableEq

/-- Deterministic bounded traversal over the checked expansion/disposal actions.
    Refusal retains the current pending work, after any completed earlier steps. -/
def runCleanup (fuel : Nat) (memory : Memory.StateOf Core.Value) (pending : List Core.Value) :
    Memory.StateOf Core.Value × CleanupOutcome × List Core.Value :=
  match pending with
  | [] => (memory, .complete, [])
  | _ :: _ =>
    match fuel with
    | 0 => (memory, .paused, pending)
    | remaining + 1 =>
      match cleanupWorkStep memory pending with
      | (next, .error reason, rest) => (next, .refused reason, rest)
      | (next, .ok (), rest) => runCleanup remaining next rest

/-- A worklist invariant survives dynamic expansion, pause and refusal when
    each checked action preserves it. This is shared by all runner proofs. -/
theorem run_cleanup_preserves
    (invariant : Memory.StateOf Core.Value → List Core.Value → Prop)
    (preserves : ∀ memory pending, invariant memory pending →
      let result := cleanupWorkStep memory pending
      invariant result.1 result.2.2)
    (fuel : Nat) (memory : Memory.StateOf Core.Value) (pending : List Core.Value)
    (known : invariant memory pending) :
    let result := runCleanup fuel memory pending
    invariant result.1 result.2.2 := by
  induction fuel generalizing memory pending with
  | zero => cases pending <;> simpa only [runCleanup] using known
  | succ fuel ih =>
    cases pending with
    | nil => simpa only [runCleanup] using known
    | cons value tail =>
      have one := preserves memory (value :: tail) known
      cases performed : cleanupWorkStep memory (value :: tail) with
      | mk next pair =>
        cases pair with
        | mk outcome rest =>
          rw [performed] at one
          cases outcome with
          | error reason => simpa only [runCleanup, performed] using one
          | ok doneValue =>
            cases doneValue
            simpa only [runCleanup, performed] using ih next rest one

theorem run_cleanup_owns (fuel : Nat) (memory : Memory.StateOf Core.Value) (pending : List Core.Value)
    (registers : List (Option Core.Value)) (owned : OwnsHeapResources (registers ++ pending.map some) memory) :
    let result := runCleanup fuel memory pending
    OwnsHeapResources (registers ++ result.2.2.map some) result.1 :=
  run_cleanup_preserves (fun state work => OwnsHeapResources (registers ++ work.map some) state)
    (fun state work valid => cleanup_work_step_owns state work registers valid) fuel memory pending owned

/-- Complete means that every pending value has been consumed. -/
theorem run_cleanup_complete_empty (fuel : Nat) (memory : Memory.StateOf Core.Value) (pending : List Core.Value)
    (complete : (runCleanup fuel memory pending).2.1 = .complete) :
    (runCleanup fuel memory pending).2.2 = [] := by
  induction fuel generalizing memory pending with
  | zero => cases pending <;> simp_all [runCleanup]
  | succ fuel ih =>
    cases pending with
    | nil => rfl
    | cons value tail =>
      cases performed : cleanupWorkStep memory (value :: tail) with
      | mk next pair =>
        cases pair with
        | mk outcome rest =>
          cases outcome with
          | error reason => simp [runCleanup, performed] at complete
          | ok doneValue =>
            cases doneValue
            simpa only [runCleanup, performed] using ih next rest (by simpa only [runCleanup, performed] using complete)

/-- The finite rank bounds successful actions: with sufficient fuel the runner
    returns complete or refused, never paused. This holds for arbitrary states;
    successful cleanup without refusal needs separate readiness obligations. -/
theorem run_cleanup_no_pause (fuel : Nat) (memory : Memory.StateOf Core.Value) (pending : List Core.Value)
    (enough : cleanupMeasure memory pending ≤ fuel) : (runCleanup fuel memory pending).2.1 ≠ .paused := by
  induction fuel generalizing memory pending with
  | zero =>
    cases pending with
    | nil => simp [runCleanup]
    | cons value tail =>
      simp only [cleanupMeasure, List.length_cons] at enough
      omega
  | succ fuel ih =>
    cases pending with
    | nil => simp [runCleanup]
    | cons value tail =>
      cases performed : cleanupWorkStep memory (value :: tail) with
      | mk next pair =>
        cases pair with
        | mk outcome rest =>
          cases outcome with
          | error reason => simp [runCleanup, performed]
          | ok doneValue =>
            cases doneValue
            have success : (cleanupWorkStep memory (value :: tail)).2.1 = .ok () := by rw [performed]
            have progress := cleanup_work_step_progress memory (value :: tail) (by simp) success
            rw [performed] at progress
            change cleanupMeasure next rest + 1 = cleanupMeasure memory (value :: tail) at progress
            have smaller : cleanupMeasure next rest ≤ fuel := by omega
            simpa only [runCleanup, performed] using ih next rest smaller

/-- Compute a sufficient logical action budget rather than exposing arbitrary
    fuel as the completed cleanup API. Refusal still returns owned pending work. -/
def cleanupHeap (memory : Memory.StateOf Core.Value) (pending : List Core.Value) :
    Memory.StateOf Core.Value × CleanupOutcome × List Core.Value :=
  runCleanup (cleanupMeasure memory pending) memory pending

theorem cleanup_heap_not_paused (memory : Memory.StateOf Core.Value) (pending : List Core.Value) :
    (cleanupHeap memory pending).2.1 ≠ .paused :=
  run_cleanup_no_pause (cleanupMeasure memory pending) memory pending (Nat.le_refl _)

theorem cleanup_heap_owns (memory : Memory.StateOf Core.Value) (pending : List Core.Value)
    (registers : List (Option Core.Value)) (owned : OwnsHeapResources (registers ++ pending.map some) memory) :
    OwnsHeapResources (registers ++ (cleanupHeap memory pending).2.2.map some) (cleanupHeap memory pending).1 :=
  run_cleanup_owns (cleanupMeasure memory pending) memory pending registers owned

end Witchy.Storage

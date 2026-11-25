import Witchy.PayloadTally
import Witchy.RecursiveCleanup

namespace Witchy.Storage

/-- Total weight in pending work and initialized heap payloads. -/
def cleanupPayloadTally (weight : Core.Value → Nat) (memory : Memory.StateOf Core.Value) (pending : List Core.Value) : Nat :=
  (pending.map weight).sum + heapPayloadTally weight memory

private theorem finish_tally_le (weight : Core.Value → Nat) (memory : Memory.StateOf Core.Value)
    (value : Core.Value) (tail : List Core.Value) :
    cleanupPayloadTally weight (finishCleanupValue memory value tail).1 (finishCleanupValue memory value tail).2.2 ≤
      cleanupPayloadTally weight memory (value :: tail) := by
  cases disposed : disposeHeapValue memory value with
  | mk next outcome =>
    cases outcome with
    | error reason => simp only [finishCleanupValue, disposed, Nat.le_refl]
    | ok doneValue =>
      cases doneValue
      have heap := dispose_payload_tally_le weight memory value
      rw [disposed] at heap
      dsimp only at heap
      simp only [finishCleanupValue, disposed, cleanupPayloadTally, List.map_cons, List.sum_cons]
      omega

private theorem expansion_tally_le (weight : Core.Value → Nat) (memory : Memory.StateOf Core.Value)
    (root index : Nat) (tail : List Core.Value) :
    cleanupPayloadTally weight (expandCleanupOwner memory root index tail).1 (expandCleanupOwner memory root index tail).2.2 ≤
      cleanupPayloadTally weight memory (.owner root :: tail) := by
  cases taken : Memory.take memory (.owner root) index with
  | mk next outcome =>
    cases outcome with
    | error reason => simp only [expandCleanupOwner, taken, Nat.le_refl]
    | ok child =>
      have accepted : (Memory.take memory (.owner root) index).2 = .ok child := by rw [taken]
      have transferred := take_payload_tally weight memory (.owner root) index child accepted
      rw [taken] at transferred
      dsimp only at transferred
      by_cases guard : cleanupPayloadUnborrowed next child = true
      · simp only [expandCleanupOwner, taken, guard, ↓reduceIte, cleanupPayloadTally, List.map_cons, List.sum_cons]
        omega
      · simp only [expandCleanupOwner, taken, guard, Bool.false_eq_true, ↓reduceIte, Nat.le_refl]

theorem cleanup_work_step_payload_tally_le (weight : Core.Value → Nat) (memory : Memory.StateOf Core.Value)
    (pending : List Core.Value) :
    cleanupPayloadTally weight (cleanupWorkStep memory pending).1 (cleanupWorkStep memory pending).2.2 ≤
      cleanupPayloadTally weight memory pending := by
  cases pending with
  | nil => exact Nat.le_refl _
  | cons value tail =>
    cases value <;> simp only [cleanupWorkStep]
    all_goals first
      | exact finish_tally_le weight memory _ tail
      | (split <;> first
          | exact finish_tally_le weight memory _ tail
          | (split <;> first
              | exact finish_tally_le weight memory _ tail
              | exact expansion_tally_le weight memory _ _ tail))

/-- Cleanup cannot amplify any nonnegative payload weighting, on any exit.
    Selecting an indicator weight gives multiplicity, rather than only origin. -/
theorem run_cleanup_payload_tally_le (weight : Core.Value → Nat) (fuel : Nat)
    (memory : Memory.StateOf Core.Value) (pending : List Core.Value) :
    cleanupPayloadTally weight (runCleanup fuel memory pending).1 (runCleanup fuel memory pending).2.2 ≤
      cleanupPayloadTally weight memory pending := by
  apply run_cleanup_preserves
    (fun state work => cleanupPayloadTally weight state work ≤ cleanupPayloadTally weight memory pending)
    _ fuel memory pending (Nat.le_refl _)
  intro state work known
  exact Nat.le_trans (cleanup_work_step_payload_tally_le weight state work) known

/-- Count this exact token identity, wherever its payload is held. -/
def capabilityWeight (token : Nat) (value : Core.Value) : Nat := if value = .capability token then 1 else 0

theorem cleanup_heap_capability_multiplicity (token : Nat) (memory : Memory.StateOf Core.Value) (pending : List Core.Value) :
    cleanupPayloadTally (capabilityWeight token) (cleanupHeap memory pending).1 (cleanupHeap memory pending).2.2 ≤
      cleanupPayloadTally (capabilityWeight token) memory pending :=
  run_cleanup_payload_tally_le (capabilityWeight token) (cleanupMeasure memory pending) memory pending

end Witchy.Storage

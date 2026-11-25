import Witchy.CleanupTerminalProgress

namespace Witchy.Storage

/-- One terminal action, retaining the current task on refusal. -/
def finishCleanupValue (memory : Memory.StateOf Core.Value) (value : Core.Value) (tail : List Core.Value) :
    Memory.StateOf Core.Value × Memory.Result Unit × List Core.Value :=
  match disposeHeapValue memory value with
  | (next, .ok ()) => (next, .ok (), tail)
  | (_, .error reason) => (memory, .error reason, value :: tail)

/-- One deterministic worklist action. Children are processed before retrying
    their parent; backing storage is released only after selection proves empty. -/
def cleanupWorkStep (memory : Memory.StateOf Core.Value) : List Core.Value →
    Memory.StateOf Core.Value × Memory.Result Unit × List Core.Value
  | [] => (memory, .ok (), [])
  | value :: tail =>
    match value with
    | .owner root =>
      match Memory.block? memory root with
      | none => finishCleanupValue memory value tail
      | some block =>
        match block.lastInitialized with
        | none => finishCleanupValue memory value tail
        | some (index, _) => expandCleanupOwner memory root index tail
    | _ => finishCleanupValue memory value tail

theorem finish_cleanup_value_owns (memory : Memory.StateOf Core.Value) (value : Core.Value)
    (tail : List Core.Value) (registers : List (Option Core.Value))
    (owned : OwnsHeapResources (registers ++ (value :: tail).map some) memory) :
    let result := finishCleanupValue memory value tail
    OwnsHeapResources (registers ++ result.2.2.map some) result.1 := by
  cases disposed : disposeHeapValue memory value with
  | mk next outcome =>
    cases outcome with
    | error reason => simpa only [finishCleanupValue, disposed] using owned
    | ok doneValue =>
      cases doneValue
      dsimp only [finishCleanupValue]
      rw [disposed]
      intro resource
      have accepted : (disposeHeapValue memory value).2 = .ok () := by rw [disposed]
      have one := dispose_heap_value_accounting memory value accepted resource
      rw [disposed] at one
      have prior := owned resource
      simp only [Core.registerCount, List.map_append, List.map_cons, List.sum_append, List.sum_cons] at *
      omega

theorem finish_cleanup_value_progress (memory : Memory.StateOf Core.Value) (value : Core.Value)
    (tail : List Core.Value) (success : (finishCleanupValue memory value tail).2.1 = .ok ()) :
    let result := finishCleanupValue memory value tail
    cleanupMeasure result.1 result.2.2 + 1 = cleanupMeasure memory (value :: tail) := by
  cases disposed : disposeHeapValue memory value with
  | mk next outcome =>
    cases outcome with
    | error reason => simp [finishCleanupValue, disposed] at success
    | ok doneValue =>
      cases doneValue
      have accepted : (disposeHeapValue memory value).2 = .ok () := by rw [disposed]
      have progress := terminal_disposal_progress memory value tail accepted
      simpa only [finishCleanupValue, disposed] using progress

theorem cleanup_work_step_owns (memory : Memory.StateOf Core.Value) (pending : List Core.Value)
    (registers : List (Option Core.Value)) (owned : OwnsHeapResources (registers ++ pending.map some) memory) :
    let result := cleanupWorkStep memory pending
    OwnsHeapResources (registers ++ result.2.2.map some) result.1 := by
  cases pending with
  | nil => exact owned
  | cons value tail =>
    cases value <;> simp only [cleanupWorkStep]
    all_goals first
      | exact finish_cleanup_value_owns memory _ tail registers owned
      | (split <;> first
          | exact finish_cleanup_value_owns memory _ tail registers owned
          | (split <;> first
              | exact finish_cleanup_value_owns memory _ tail registers owned
              | exact expand_cleanup_owner_owns memory _ _ tail registers owned))

theorem cleanup_work_step_progress (memory : Memory.StateOf Core.Value) (pending : List Core.Value)
    (nonempty : pending ≠ []) (success : (cleanupWorkStep memory pending).2.1 = .ok ()) :
    let result := cleanupWorkStep memory pending
    cleanupMeasure result.1 result.2.2 + 1 = cleanupMeasure memory pending := by
  cases pending with
  | nil => exact False.elim (nonempty rfl)
  | cons value tail =>
    cases value with
    | owner root =>
      cases stored : Memory.block? memory root with
      | none =>
        simpa only [cleanupWorkStep, stored] using finish_cleanup_value_progress memory (.owner root) tail
          (by simpa only [cleanupWorkStep, stored] using success)
      | some block =>
        cases selected : block.lastInitialized with
        | none =>
          simpa only [cleanupWorkStep, stored, selected] using finish_cleanup_value_progress memory (.owner root) tail
            (by simpa only [cleanupWorkStep, stored, selected] using success)
        | some pair =>
          obtain ⟨index, child⟩ := pair
          simpa only [cleanupWorkStep, stored, selected] using owner_expansion_success_progress memory root index tail
            (by simpa only [cleanupWorkStep, stored, selected] using success)
    | scalarAggregate value => exact finish_cleanup_value_progress memory (.scalarAggregate value) tail success
    | number value => exact finish_cleanup_value_progress memory (.number value) tail success
    | boolean value => exact finish_cleanup_value_progress memory (.boolean value) tail success
    | reference identity => exact finish_cleanup_value_progress memory (.reference identity) tail success
    | capability token => exact finish_cleanup_value_progress memory (.capability token) tail success

end Witchy.Storage

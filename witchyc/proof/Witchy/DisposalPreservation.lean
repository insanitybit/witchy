import Witchy.ResourceDisposal

namespace Witchy.Storage

/-- A reusable invariant rule for the shared cleanup walker, including refusal.
    Per-value preservation is proved independently of traversal length. -/
theorem dispose_values_using_preserves
    (disposeOne : Memory.StateOf Cell → Core.Value → Memory.StateOf Cell × Memory.Result Unit)
    (invariant : Memory.StateOf Cell → Prop)
    (preserves : ∀ memory value, invariant memory → invariant (disposeOne memory value).1)
    (memory : Memory.StateOf Cell) (values : List Core.Value) (valid : invariant memory) :
    invariant (disposeValuesUsing disposeOne memory values).1 := by
  induction values generalizing memory with
  | nil => exact valid
  | cons value tail ih =>
    have next := preserves memory value valid
    cases answer : (disposeOne memory value).2 with
    | ok doneValue =>
      cases doneValue
      simpa only [disposeValuesUsing, answer] using ih (disposeOne memory value).1 next
    | error reason => simpa only [disposeValuesUsing, answer] using next

/-- The pending output contains only original payloads, for any cleanup policy.
    This supports authority validity without reconstructing the traversal. -/
theorem dispose_values_using_pending_subset
    (disposeOne : Memory.StateOf Cell → Core.Value → Memory.StateOf Cell × Memory.Result Unit)
    (memory : Memory.StateOf Cell) (values : List Core.Value) :
    ∀ value ∈ (disposeValuesUsing disposeOne memory values).2.2, value ∈ values := by
  induction values generalizing memory with
  | nil => simp [disposeValuesUsing]
  | cons head tail ih =>
    cases answer : (disposeOne memory head).2 with
    | ok doneValue =>
      cases doneValue
      intro value member
      have prior := ih (disposeOne memory head).1 value (by simpa only [disposeValuesUsing, answer] using member)
      exact List.mem_cons_of_mem head prior
    | error reason => simpa only [disposeValuesUsing, answer] using fun value (member : value ∈ head :: tail) => member

end Witchy.Storage

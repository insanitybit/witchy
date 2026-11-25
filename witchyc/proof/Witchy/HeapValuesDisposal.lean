import Witchy.HeapValueDisposal

namespace Witchy.Storage

/-- Checked empty-owner/loan cleanup over the shared walker. Stop at the first
    refusal and retain the exact refused value and remaining suffix. Nested
    initialized allocations must be drained before this policy can release them. -/
def disposeHeapValues (memory : Memory.StateOf Core.Value) (values : List Core.Value) :
    Memory.StateOf Core.Value × Memory.Result Unit × List Core.Value :=
  disposeValuesUsing disposeHeapValue memory values

/-- Every resource removed from live declarations belongs to the successfully
    disposed prefix. The pending suffix retains all of its obligations. -/
theorem dispose_heap_values_accounting (memory : Memory.StateOf Core.Value) (values : List Core.Value)
    (resource : Core.Resource) :
    Core.memoryCount (disposeHeapValues memory values).1 resource + Core.registerCount (values.map some) resource =
      Core.memoryCount memory resource + Core.registerCount ((disposeHeapValues memory values).2.2.map some) resource ∧
    heapResources (disposeHeapValues memory values).1 resource = heapResources memory resource := by
  induction values generalizing memory with
  | nil => simp [disposeHeapValues, disposeValuesUsing, Core.registerCount]
  | cons value tail ih =>
    cases answer : (disposeHeapValue memory value).2 with
    | ok doneValue =>
      cases doneValue
      have one := dispose_heap_value_accounting memory value answer resource
      have rest := ih (disposeHeapValue memory value).1
      simp only [disposeHeapValues, disposeValuesUsing, answer]
      change (Core.memoryCount (disposeHeapValues (disposeHeapValue memory value).1 tail).1 resource +
        (Core.weight resource (some value) + Core.registerCount (tail.map some) resource) =
        Core.memoryCount memory resource +
          Core.registerCount ((disposeHeapValues (disposeHeapValue memory value).1 tail).2.2.map some) resource) ∧
        heapResources (disposeHeapValues (disposeHeapValue memory value).1 tail).1 resource = heapResources memory resource
      constructor <;> omega
    | error reason =>
      have unchanged := dispose_heap_value_refused memory value reason answer
      simp [disposeHeapValues, disposeValuesUsing, answer, unchanged]

/-- Abstract caller storage plus the pending values still owns the resulting
    heap, on success and refusal alike. No successful cleanup is assumed. -/
theorem dispose_heap_values_owns (memory : Memory.StateOf Core.Value) (values : List Core.Value)
    (registers : List (Option Core.Value))
    (owned : OwnsHeapResources (registers ++ values.map some) memory) :
    OwnsHeapResources (registers ++ (disposeHeapValues memory values).2.2.map some)
      (disposeHeapValues memory values).1 := by
  intro resource
  have prior := owned resource
  have disposed := dispose_heap_values_accounting memory values resource
  simp only [Core.registerCount, List.map_append, List.sum_append] at *
  omega

theorem dispose_heap_values_success_empty (memory : Memory.StateOf Core.Value) (values : List Core.Value)
    (accepted : (disposeHeapValues memory values).2.1 = .ok ()) :
    (disposeHeapValues memory values).2.2 = [] :=
  dispose_values_using_success_empty disposeHeapValue memory values accepted

/-- A refusal retains the current payload and all unprocessed values verbatim. -/
theorem dispose_heap_values_refused_head (memory : Memory.StateOf Core.Value) (value : Core.Value)
    (tail : List Core.Value) (reason : Memory.Error)
    (refused : (disposeHeapValue memory value).2 = .error reason) :
    disposeHeapValues memory (value :: tail) = (memory, .error reason, value :: tail) := by
  simp [disposeHeapValues, disposeValuesUsing, refused, dispose_heap_value_refused memory value reason refused]

end Witchy.Storage

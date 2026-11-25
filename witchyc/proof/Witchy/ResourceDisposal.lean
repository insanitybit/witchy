import Witchy.ResourceMemory

namespace Witchy.Storage

/-- Shared value dispatch with an explicit allocation-disposition operation.
    A policy's safety follows from its checked implementation and proofs. -/
def disposeValueUsing (releaseOwner : Memory.StateOf Cell → Memory.Handle →
    Memory.StateOf Cell × Memory.Result Unit) (memory : Memory.StateOf Cell) :
    Core.Value → Memory.StateOf Cell × Memory.Result Unit
  | .owner handle => releaseOwner memory handle
  | .reference identity => Memory.endLoan memory identity
  | _ => (memory, .ok ())

/-- Compatibility policy: existing owner allocations contain scalar cells.
    Resource-bearing heaps use checked empty release instead. -/
def disposeValue (memory : Memory.State) (value : Core.Value) : Memory.State × Memory.Result Unit :=
  disposeValueUsing Memory.release memory value

theorem dispose_value_accounting (memory : Memory.State) (value : Core.Value)
    (accepted : (disposeValue memory value).2 = .ok ()) (resource : Core.Resource) :
    Core.memoryCount (disposeValue memory value).1 resource + Core.weight resource (some value) =
      Core.memoryCount memory resource := by
  cases value with
  | owner handle => exact Core.release_resource_accounting memory handle accepted resource
  | reference identity => exact Core.end_resource_accounting memory identity accepted resource
  | number value => simp [disposeValue, disposeValueUsing, Core.weight, Core.valueResource]
  | boolean value => simp [disposeValue, disposeValueUsing, Core.weight, Core.valueResource]
  | capability token | scalarAggregate token => simp [disposeValue, disposeValueUsing, Core.weight, Core.valueResource]

/-- Refused disposal retains its entire memory state. Its payload must be
    returned by the enclosing API; a refusal is never treated as a completed drop. -/
theorem dispose_value_refused (memory : Memory.State) (value : Core.Value) (reason : Memory.Error)
    (failed : (disposeValue memory value).2 = .error reason) : (disposeValue memory value).1 = memory := by
  cases value with
  | owner handle =>
    change (Memory.release memory handle).2 = .error reason at failed
    change (Memory.release memory handle).1 = memory
    cases found : Memory.block? memory handle with
    | none => simp [Memory.release, found]
    | some block =>
      by_cases free : (Memory.active memory handle).isEmpty = true
      · simp [Memory.release, found, free] at failed
      · simp [Memory.release, found, free]
  | reference identity => exact Memory.end_failure_atomic memory identity reason failed
  | number value => simp [disposeValue, disposeValueUsing] at failed
  | boolean value => simp [disposeValue, disposeValueUsing] at failed
  | capability token | scalarAggregate token => simp [disposeValue, disposeValueUsing] at failed

theorem dispose_value_quota (memory : Memory.State) (value : Core.Value) :
    Memory.quota (disposeValue memory value).1 = Memory.quota memory := by
  cases value with
  | owner handle => exact Memory.release_preserves_quota memory handle
  | reference identity => exact Memory.end_preserves_quota memory identity
  | number value => rfl
  | boolean value => rfl
  | capability token | scalarAggregate token => rfl

/-- Stop on the first refusal and return the current and unprocessed values.
    The completed prefix has actually been disposed, not merely forgotten. -/
def disposeValuesUsing (disposeOne : Memory.StateOf Cell → Core.Value →
    Memory.StateOf Cell × Memory.Result Unit) (memory : Memory.StateOf Cell) :
    List Core.Value → Memory.StateOf Cell × Memory.Result Unit × List Core.Value
  | [] => (memory, .ok (), [])
  | value :: tail =>
    let step := disposeOne memory value
    match step.2 with
    | .ok () => disposeValuesUsing disposeOne step.1 tail
    | .error reason => (step.1, .error reason, value :: tail)

/-- Successful traversal has no pending payloads, for every checked policy. -/
theorem dispose_values_using_success_empty
    (disposeOne : Memory.StateOf Cell → Core.Value → Memory.StateOf Cell × Memory.Result Unit)
    (memory : Memory.StateOf Cell) (values : List Core.Value)
    (accepted : (disposeValuesUsing disposeOne memory values).2.1 = .ok ()) :
    (disposeValuesUsing disposeOne memory values).2.2 = [] := by
  induction values generalizing memory with
  | nil => rfl
  | cons value tail ih =>
    cases answer : (disposeOne memory value).2 with
    | ok doneValue =>
      cases doneValue
      simp only [disposeValuesUsing, answer] at accepted ⊢
      exact ih (disposeOne memory value).1 accepted
    | error reason => simp [disposeValuesUsing, answer] at accepted

/-- Existing scalar policy over the shared stop-on-refusal walker. -/
def disposeValues (memory : Memory.State) (values : List Core.Value) :
    Memory.State × Memory.Result Unit × List Core.Value :=
  disposeValuesUsing disposeValue memory values

end Witchy.Storage

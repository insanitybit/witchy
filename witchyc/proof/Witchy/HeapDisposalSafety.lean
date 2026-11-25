import Witchy.HeapValuesDisposal
import Witchy.DisposalPreservation
import Witchy.HeapOwnershipGraph
import Witchy.HeapCapabilityTransfer

namespace Witchy.Storage

/-- Checked disposal preserves capability-table validity even when refused. -/
theorem dispose_heap_value_capabilities (memory : Memory.StateOf Core.Value) (value : Core.Value)
    (authority : Authority.State) (valid : heapCapabilitiesValid authority memory) :
    heapCapabilitiesValid authority (disposeHeapValue memory value).1 := by
  cases value with
  | owner handle =>
    cases answer : (Memory.releaseEmpty memory handle).2 with
    | ok doneValue =>
      cases doneValue
      obtain ⟨block, _stored, _free, _empty, effect⟩ := Memory.release_empty_safe memory handle answer
      change heapCapabilitiesValid authority (Memory.releaseEmpty memory handle).1
      rw [effect]
      exact release_heap_capabilities authority memory handle valid
    | error reason =>
      change heapCapabilitiesValid authority (Memory.releaseEmpty memory handle).1
      rw [Memory.release_empty_refused memory handle reason answer]
      exact valid
  | reference identity =>
    change heapCapabilitiesValid authority (Memory.endLoan memory identity).1
    simpa only [heapCapabilitiesValid, Memory.end_loan_blocks] using valid
  | number value => exact valid
  | boolean value => exact valid
  | capability token | scalarAggregate token => exact valid

/-- Cleanup does not introduce ownership cycles, on success or refusal. -/
theorem dispose_heap_value_wellFounded (memory : Memory.StateOf Core.Value) (value : Core.Value)
    (valid : OwnershipWellFounded memory) : OwnershipWellFounded (disposeHeapValue memory value).1 := by
  cases value with
  | owner handle =>
    cases answer : (Memory.releaseEmpty memory handle).2 with
    | ok doneValue =>
      cases doneValue
      exact release_ownership_wellFounded memory handle valid answer
    | error reason =>
      change OwnershipWellFounded (Memory.releaseEmpty memory handle).1
      rw [Memory.release_empty_refused memory handle reason answer]
      exact valid
  | reference identity =>
    change OwnershipWellFounded (Memory.endLoan memory identity).1
    unfold OwnershipWellFounded
    rw [ownership_blocks_frame memory _ (Memory.end_loan_blocks memory identity)]
    exact valid
  | number value => exact valid
  | boolean value => exact valid
  | capability token | scalarAggregate token => exact valid

/-- Full finite traversal retains structural ownership with no assumed success. -/
theorem dispose_heap_values_wellFounded (memory : Memory.StateOf Core.Value) (values : List Core.Value)
    (valid : OwnershipWellFounded memory) : OwnershipWellFounded (disposeHeapValues memory values).1 :=
  dispose_values_using_preserves disposeHeapValue OwnershipWellFounded dispose_heap_value_wellFounded memory values valid

/-- Caller storage and pending payloads retain capability validity against the
    same authority table. No authority entry is created or strengthened. -/
theorem dispose_heap_values_capabilities (memory : Memory.StateOf Core.Value) (values : List Core.Value)
    (authority : Authority.State) (registers : List (Option Core.Value))
    (valid : allCapabilitiesValid authority (registers ++ values.map some) memory) :
    allCapabilitiesValid authority (registers ++ (disposeHeapValues memory values).2.2.map some)
      (disposeHeapValues memory values).1 := by
  refine ⟨?_, dispose_values_using_preserves disposeHeapValue (heapCapabilitiesValid authority)
    (fun state value known => dispose_heap_value_capabilities state value authority known) memory values valid.2⟩
  intro value member
  apply valid.1 value
  rcases List.mem_append.mp member with caller | pending
  · exact List.mem_append_left _ caller
  · have original : value ∈ values := by
      have pendingValue : value ∈ (disposeHeapValues memory values).2.2 := by simpa using pending
      exact dispose_values_using_pending_subset disposeHeapValue memory values value pendingValue
    exact List.mem_append_right _ (by simpa using original)

end Witchy.Storage

import Witchy.ResourceDisposal
import Witchy.HeapResourceRelease
import Witchy.MemoryBlockFrame

namespace Witchy.Storage

/-- An owner can be disposed only after its allocation has no initialized
    payloads. Nested cleanup must discharge those payloads first. A refused
    owner remains the caller's obligation. -/
def disposeHeapValue (memory : Memory.StateOf Core.Value) (value : Core.Value) :
    Memory.StateOf Core.Value × Memory.Result Unit :=
  disposeValueUsing (fun state handle => Memory.releaseEmpty state handle) memory value

/-- Successful heap-value disposal removes exactly the payload's live resource
    declaration and no stored payload resources. -/
theorem dispose_heap_value_accounting (memory : Memory.StateOf Core.Value) (value : Core.Value)
    (accepted : (disposeHeapValue memory value).2 = .ok ()) (resource : Core.Resource) :
    Core.memoryCount (disposeHeapValue memory value).1 resource + Core.weight resource (some value) =
      Core.memoryCount memory resource ∧
    heapResources (disposeHeapValue memory value).1 resource = heapResources memory resource := by
  cases value with
  | owner handle =>
    exact ⟨release_empty_live_inventory memory handle accepted resource,
      release_empty_heap_inventory memory handle accepted resource⟩
  | reference identity =>
    refine ⟨Core.end_resource_accounting memory identity accepted resource, ?_⟩
    unfold disposeHeapValue disposeValueUsing
    simp only [heapResources, Memory.end_loan_blocks]
  | number value => simp [disposeHeapValue, disposeValueUsing, Core.weight, Core.valueResource]
  | boolean value => simp [disposeHeapValue, disposeValueUsing, Core.weight, Core.valueResource]
  | capability token | scalarAggregate token => simp [disposeHeapValue, disposeValueUsing, Core.weight, Core.valueResource]

/-- Caller storage retains exact ownership after consuming a successfully
    disposed payload. This accounts for allocations and loan identities alike. -/
theorem dispose_heap_value_owns (memory : Memory.StateOf Core.Value) (value : Core.Value)
    (registers : List (Option Core.Value)) (owned : OwnsHeapResources (some value :: registers) memory)
    (accepted : (disposeHeapValue memory value).2 = .ok ()) :
    OwnsHeapResources registers (disposeHeapValue memory value).1 := by
  intro resource
  have prior := owned resource
  have disposed := dispose_heap_value_accounting memory value accepted resource
  simp only [Core.registerCount, List.map_cons, List.sum_cons] at *
  omega

/-- Refusal never publishes a partial change or consumes the refused payload. -/
theorem dispose_heap_value_refused (memory : Memory.StateOf Core.Value) (value : Core.Value)
    (reason : Memory.Error) (refused : (disposeHeapValue memory value).2 = .error reason) :
    (disposeHeapValue memory value).1 = memory := by
  cases value with
  | owner handle => exact Memory.release_empty_refused memory handle reason refused
  | reference identity => exact Memory.end_failure_atomic memory identity reason refused
  | number value => simp [disposeHeapValue, disposeValueUsing] at refused
  | boolean value => simp [disposeHeapValue, disposeValueUsing] at refused
  | capability token | scalarAggregate token => simp [disposeHeapValue, disposeValueUsing] at refused

/-- An initialized nested allocation cannot be discarded by this disposition
    policy, even when no loan prevents its release. -/
theorem dispose_heap_owner_initialized (memory : Memory.StateOf Core.Value) (handle : Memory.Handle)
    (block : Block Core.Value) (value : Core.Value) (stored : Memory.block? memory handle = some block)
    (initialized : some value ∈ block.slots) (free : (Memory.active memory handle).isEmpty = true) :
    disposeHeapValue memory (.owner handle) = (memory, .error .initialized) :=
  Memory.release_empty_initialized memory handle block value stored initialized free

end Witchy.Storage

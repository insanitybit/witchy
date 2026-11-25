import Witchy.HeapResources
import Witchy.ResourceMemory

namespace Witchy.Storage

theorem initial_heap_ownership (limits : Memory.Limits) :
    OwnsHeapResources [] (Memory.initialOf Core.Value limits) := by
  intro resource
  cases resource <;> simp [Core.registerCount, heapResources, Core.memoryCount, Memory.initialOf, Memory.block?, Memory.loan?]

theorem allocation_empty_inventory (memory : Memory.StateOf Core.Value) (count handle : Nat)
    (accepted : (Memory.allocate memory count).2 = .ok handle) (resource : Core.Resource) :
    heapResources (Memory.allocate memory count).1 resource = heapResources memory resource := by
  unfold Memory.allocate at accepted ⊢
  split at accepted
  · split at accepted
    · cases accepted
      simp_all [heapResources, blockResources, Core.registerCount, Core.weight]
    · cases accepted
  · cases accepted

/-- Allocation introduces exactly its fresh root owner into a vacant register;
    uninitialized cells introduce no nested payload resources. -/
theorem allocate_to_register_owns (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (count handle destination : Nat) (owned : OwnsHeapResources registers memory)
    (vacant : registers[destination]? = some none) (accepted : (Memory.allocate memory count).2 = .ok handle) :
    OwnsHeapResources (registers.set destination (some (.owner handle))) (Memory.allocate memory count).1 := by
  intro resource
  have prior := owned resource
  have storage := allocation_empty_inventory memory count handle accepted resource
  have live := Core.allocation_resource_accounting memory count handle accepted resource
  have inserted := Core.register_set_accounting registers destination none (some (.owner handle)) resource vacant
  have zero : Core.weight resource none = 0 := by simp [Core.weight]
  rw [zero] at inserted
  rw [storage, live]
  omega

end Witchy.Storage

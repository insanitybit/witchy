import Witchy.HeapResources
import Witchy.GenericMemoryRelease

namespace Witchy.Storage

private theorem empty_cells_resources (cells : List (Option Core.Value)) (resource : Core.Resource)
    (empty : ∀ cell ∈ cells, cell = none) : Core.registerCount cells resource = 0 := by
  induction cells with
  | nil => rfl
  | cons head tail ih =>
    have headEmpty := empty head (by simp)
    have tailEmpty := ih (fun cell member => empty cell (by simp [member]))
    change Core.weight resource head + Core.registerCount tail resource = 0
    simp [headEmpty, tailEmpty, Core.weight]

/-- Empty release removes no payload resources from any heap block. -/
theorem release_empty_heap_inventory (memory : Memory.StateOf Core.Value) (handle : Memory.Handle)
    (accepted : (Memory.releaseEmpty memory handle).2 = .ok ()) (resource : Core.Resource) :
    heapResources (Memory.releaseEmpty memory handle).1 resource = heapResources memory resource := by
  obtain ⟨block, found, _free, empty, effect⟩ := Memory.release_empty_safe memory handle accepted
  have zero := empty_cells_resources block.slots resource empty
  have inventory := sum_set_weight memory.blocks handle (some block) none (blockResources resource)
    (Memory.block_lookup memory handle block found)
  rw [effect]
  change ((memory.blocks.set handle none).map (blockResources resource)).sum = _
  simpa [heapResources, blockResources, zero] using inventory

/-- The allocation identity is consumed exactly once, including allocations
    with no slots. Loan identities and unrelated allocation identities remain. -/
theorem release_empty_live_inventory (memory : Memory.StateOf Core.Value) (handle : Memory.Handle)
    (accepted : (Memory.releaseEmpty memory handle).2 = .ok ()) (resource : Core.Resource) :
    Core.memoryCount (Memory.releaseEmpty memory handle).1 resource + Core.weight resource (some (.owner handle)) =
      Core.memoryCount memory resource := by
  obtain ⟨block, found, _free, _empty, effect⟩ := Memory.release_empty_safe memory handle accepted
  cases resource with
  | loan identity =>
    rw [effect]
    simp [Core.memoryCount, Memory.loan?, Core.weight, Core.valueResource]
  | allocation other =>
    by_cases same : handle = other
    · subst other
      have bound := (List.getElem?_eq_some_iff.mp (Memory.block_lookup memory handle block found)).1
      have gone : Memory.block? (Memory.releaseEmpty memory handle).1 handle = none := by
        rw [effect]
        simp [Memory.block?, List.getElem?_set_self bound]
      simp [Core.memoryCount, gone, found, Core.weight, Core.valueResource]
    · have unchanged : Memory.block? (Memory.releaseEmpty memory handle).1 other = Memory.block? memory other := by
        rw [effect]
        simp only [Memory.block?, List.getElem?_set_ne same]
      simp [Core.memoryCount, unchanged, Core.weight, Core.valueResource, same]

theorem release_from_register_owns (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (source handle : Nat) (owned : OwnsHeapResources registers memory)
    (present : registers[source]? = some (some (.owner handle)))
    (accepted : (Memory.releaseEmpty memory handle).2 = .ok ()) :
    OwnsHeapResources (registers.set source none) (Memory.releaseEmpty memory handle).1 := by
  intro resource
  have prior := owned resource
  have storage := release_empty_heap_inventory memory handle accepted resource
  have live := release_empty_live_inventory memory handle accepted resource
  have moved := Core.register_set_accounting registers source (some (.owner handle)) none resource present
  have zero : Core.weight resource none = 0 := by simp [Core.weight]
  rw [zero] at moved
  rw [storage]
  omega

theorem release_refused_owns (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (handle : Memory.Handle) (reason : Memory.Error) (owned : OwnsHeapResources registers memory)
    (refused : (Memory.releaseEmpty memory handle).2 = .error reason) :
    OwnsHeapResources registers (Memory.releaseEmpty memory handle).1 := by
  rw [Memory.release_empty_refused memory handle reason refused]
  exact owned

end Witchy.Storage

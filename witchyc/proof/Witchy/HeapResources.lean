import Witchy.Resources
import Witchy.MemoryContents

namespace Witchy.Storage

def blockResources (resource : Core.Resource) (cell : Option (Block Core.Value)) : Nat :=
  (cell.map (fun block => Core.registerCount block.slots resource)).getD 0

def heapResources (memory : Memory.StateOf Core.Value) (resource : Core.Resource) : Nat :=
  (memory.blocks.map (blockResources resource)).sum

/-- Inventory ownership for the generic heap. Borrow validity, capability
    validity, reachability and relocation are separate invariants. -/
def OwnsHeapResources (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value) : Prop :=
  ∀ resource, Core.registerCount registers resource + heapResources memory resource = Core.memoryCount memory resource

theorem replace_block_inventory (memory : Memory.StateOf Core.Value) (handle : Memory.Handle)
    (old replacement : Block Core.Value) (found : Memory.block? memory handle = some old) (resource : Core.Resource) :
    heapResources { memory with blocks := memory.blocks.set handle (some replacement) } resource +
      Core.registerCount old.slots resource =
      heapResources memory resource + Core.registerCount replacement.slots resource := by
  have equation := sum_set_weight memory.blocks handle (some old) (some replacement) (blockResources resource)
    (Memory.block_lookup memory handle old found)
  simpa [heapResources, blockResources] using equation

theorem replace_block_live_count (memory : Memory.StateOf Core.Value) (handle : Memory.Handle)
    (old replacement : Block Core.Value) (found : Memory.block? memory handle = some old) (resource : Core.Resource) :
    Core.memoryCount { memory with blocks := memory.blocks.set handle (some replacement) } resource =
      Core.memoryCount memory resource := by
  cases resource with
  | loan identity => rfl
  | allocation other =>
    by_cases same : handle = other
    · subst other
      simp [Core.memoryCount, Memory.block_after_set memory handle old replacement found, found]
    · simp only [Core.memoryCount, Memory.block?, List.getElem?_set_ne same]
      rfl

theorem owned_heap_at_most_one (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (owned : OwnsHeapResources registers memory) (resource : Core.Resource) :
    Core.registerCount registers resource + heapResources memory resource ≤ 1 := by
  rw [owned resource]
  exact Core.memory_count_at_most_one memory resource

end Witchy.Storage

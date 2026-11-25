import Witchy.HeapOwnershipGraph
import Witchy.HeapCapabilities

namespace Witchy.Storage

/-- A fresh uninitialized block introduces no ownership edges. Refusal changes
    no block, so both allocator outcomes preserve the structural invariant. -/
theorem allocation_ownership_wellFounded (memory : Memory.StateOf Core.Value) (count : Nat)
    (known : OwnershipWellFounded memory) : OwnershipWellFounded (Memory.allocate memory count).1 := by
  unfold Memory.allocate
  repeat' first | exact known | split
  apply ownership_subrelation memory _ known
  intro child parent edge
  obtain ⟨block, found, member⟩ := edge
  by_cases before : parent < memory.blocks.length
  · exact ⟨block, by simpa only [Memory.block?, List.getElem?_append_left before] using found, member⟩
  · have after : memory.blocks.length ≤ parent := Nat.le_of_not_gt before
    simp only [Memory.block?, List.getElem?_append_right after] at found
    cases offset : parent - memory.blocks.length with
    | zero =>
      simp [offset] at found
      cases found
      simp at member
    | succ index => simp [offset] at found

theorem allocation_heap_capabilities (authority : Authority.State) (memory : Memory.StateOf Core.Value)
    (count : Nat) (known : heapCapabilitiesValid authority memory) :
    heapCapabilitiesValid authority (Memory.allocate memory count).1 := by
  unfold Memory.allocate
  repeat' first | exact known | split
  intro block member
  simp only [List.mem_append, List.mem_singleton] at member
  rcases member with old | same
  · exact known block old
  · cases same
    intro value stored
    simp at stored

end Witchy.Storage

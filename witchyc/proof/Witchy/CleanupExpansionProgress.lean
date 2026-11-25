import Witchy.BlockCleanupSelection
import Witchy.CleanupOwnerExpansionReady
import Witchy.CoreHeapDisposal

namespace Witchy.Storage

/-- Logical initialization count, independent of allocation identity, ownership
    resources and physical byte layout. -/
def heapInitializedCells (memory : Memory.StateOf Value) : Nat :=
  (memory.blocks.map fun cell => (cell.map Block.initializedCount).getD 0).sum

theorem heap_clear_initialized_progress (memory : Memory.StateOf Value) (root index : Nat)
    (block : Block Value) (value : Value) (stored : Memory.block? memory root = some block)
    (initialized : block.slots[index]? = some (some value)) :
    heapInitializedCells { memory with blocks := memory.blocks.set root (some ⟨block.slots.set index none⟩) } + 1 =
      heapInitializedCells memory := by
  have total := sum_set_weight memory.blocks root (some block)
    (some (⟨block.slots.set index none⟩ : Block Value))
    (fun cell => (cell.map Block.initializedCount).getD 0) (Memory.block_lookup memory root block stored)
  have one := Block.clear_initialized_progress block index value initialized
  change heapInitializedCells { memory with blocks := memory.blocks.set root (some ⟨block.slots.set index none⟩) } +
    block.initializedCount = heapInitializedCells memory +
      Block.initializedCount (⟨block.slots.set index none⟩ : Block Value) at total
  omega

/-- Extraction moves one initialized cell to work, so its heap weight is two
    and its pending-work weight is one. Terminal disposal will remove work. -/
def cleanupMeasure (memory : Memory.StateOf Core.Value) (pending : List Core.Value) : Nat :=
  2 * heapInitializedCells memory + pending.length

/-- Selecting any initialized child and successfully expanding its parent
    decreases the finite cleanup measure by exactly one. -/
theorem selected_owner_expansion_progress (memory : Memory.StateOf Core.Value) (root index : Nat)
    (block : Block Core.Value) (child : Core.Value) (tail : List Core.Value)
    (stored : Memory.block? memory root = some block)
    (selected : block.lastInitialized = some (index, child))
    (free : (Memory.active memory root).isEmpty = true)
    (eligible : Core.TransferUnborrowed memory child) :
    let result := expandCleanupOwner memory root index tail
    cleanupMeasure result.1 result.2.2 + 1 = cleanupMeasure memory (.owner root :: tail) := by
  have initialized := Block.last_initialized_present block index child selected
  have expanded := expand_cleanup_owner_ready memory root index block child tail stored free initialized eligible
  have progress := heap_clear_initialized_progress memory root index block child stored initialized
  dsimp only
  rw [expanded]
  simp only [cleanupMeasure, List.length_cons]
  omega

/-- Successful expansion itself proves progress, including when readiness was
    established by the checked primitive rather than supplied as a premise. -/
theorem owner_expansion_success_progress (memory : Memory.StateOf Core.Value) (root index : Nat)
    (tail : List Core.Value) (success : (expandCleanupOwner memory root index tail).2.1 = .ok ()) :
    let result := expandCleanupOwner memory root index tail
    cleanupMeasure result.1 result.2.2 + 1 = cleanupMeasure memory (.owner root :: tail) := by
  cases taken : Memory.take memory (.owner root) index with
  | mk next outcome =>
    cases outcome with
    | error reason => simp [expandCleanupOwner, taken] at success
    | ok child =>
      by_cases guard : cleanupPayloadUnborrowed next child = true
      · obtain ⟨handle, block, _permission, stored, initialized, effect⟩ :=
          Memory.take_cell_effect memory next (.owner root) index child taken
        have progress := heap_clear_initialized_progress memory handle index block child stored initialized
        rw [← effect] at progress
        simp only [expandCleanupOwner, taken, guard, ↓reduceIte, cleanupMeasure, List.length_cons]
        omega
      · simp [expandCleanupOwner, taken, guard] at success

/-- No selected child derives checked empty-owner readiness from permission. -/
theorem owner_ready_of_no_initialized (memory : Memory.StateOf Core.Value) (root : Nat)
    (block : Block Core.Value) (stored : Memory.block? memory root = some block)
    (selected : block.lastInitialized = none) (free : (Memory.active memory root).isEmpty = true) :
    HeapDisposalReady memory (.owner root) :=
  ⟨block, stored, free, (Block.last_initialized_none block).mp selected⟩

end Witchy.Storage

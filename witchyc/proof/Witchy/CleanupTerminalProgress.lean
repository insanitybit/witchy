import Witchy.CleanupExpansionProgress

namespace Witchy.Storage

/-- Successful terminal disposition removes no initialized heap cells: owner
    release is checked empty, and other leaf cases preserve blocks. -/
theorem dispose_heap_value_initialized_count (memory : Memory.StateOf Core.Value) (value : Core.Value)
    (accepted : (disposeHeapValue memory value).2 = .ok ()) :
    heapInitializedCells (disposeHeapValue memory value).1 = heapInitializedCells memory := by
  cases value with
  | owner handle =>
    obtain ⟨block, stored, _free, empty, effect⟩ := Memory.release_empty_safe memory handle accepted
    have zero := Block.initialized_count_empty block empty
    have total := sum_set_weight memory.blocks handle (some block) none
      (fun cell => (cell.map Block.initializedCount).getD 0) (Memory.block_lookup memory handle block stored)
    change heapInitializedCells (Memory.releaseEmpty memory handle).1 = heapInitializedCells memory
    rw [effect]
    change heapInitializedCells { memory with blocks := memory.blocks.set handle none } + block.initializedCount =
      heapInitializedCells memory + 0 at total
    simpa only [zero, Nat.add_zero, heapInitializedCells] using total
  | reference identity =>
    change heapInitializedCells (Memory.endLoan memory identity).1 = _
    simp only [heapInitializedCells, Memory.end_loan_blocks]
  | scalarAggregate fields => rfl
  | number value => rfl
  | boolean value => rfl
  | capability token => rfl

/-- Consuming one terminal cleanup task decreases the same measure as owner
    expansion. Both kinds of successful work therefore make finite progress. -/
theorem terminal_disposal_progress (memory : Memory.StateOf Core.Value) (value : Core.Value)
    (tail : List Core.Value) (accepted : (disposeHeapValue memory value).2 = .ok ()) :
    cleanupMeasure (disposeHeapValue memory value).1 tail + 1 = cleanupMeasure memory (value :: tail) := by
  simp only [cleanupMeasure, dispose_heap_value_initialized_count memory value accepted, List.length_cons]
  omega

end Witchy.Storage

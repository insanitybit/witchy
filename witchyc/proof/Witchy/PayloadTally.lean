import Witchy.HeapValueDisposal
import Witchy.MemoryCellTransfer
import Witchy.MemoryBufferInsertion

namespace Witchy.Storage

/-- Arbitrary nonnegative payload weight; vacant cells contribute zero. -/
def payloadTally (weight : Core.Value → Nat) (cells : List (Option Core.Value)) : Nat :=
  (cells.map (fun cell => cell.elim 0 weight)).sum

def heapPayloadTally (weight : Core.Value → Nat) (memory : Memory.StateOf Core.Value) : Nat :=
  (memory.blocks.map (fun cell => (cell.map (fun block => payloadTally weight block.slots)).getD 0)).sum

theorem payload_tally_set (weight : Core.Value → Nat) (cells : List (Option Core.Value)) (index : Nat)
    (old replacement : Option Core.Value) (present : cells[index]? = some old) :
    payloadTally weight (cells.set index replacement) + old.elim 0 weight =
      payloadTally weight cells + replacement.elim 0 weight :=
  sum_set_weight cells index old replacement (fun cell => cell.elim 0 weight) present

private theorem replace_tally (weight : Core.Value → Nat) (memory : Memory.StateOf Core.Value) (root : Nat)
    (old replacement : Block Core.Value) (stored : Memory.block? memory root = some old) :
    heapPayloadTally weight { memory with blocks := memory.blocks.set root (some replacement) } +
      payloadTally weight old.slots = heapPayloadTally weight memory + payloadTally weight replacement.slots := by
  exact sum_set_weight memory.blocks root (some old) (some replacement)
    (fun cell => (cell.map (fun block => payloadTally weight block.slots)).getD 0)
    (Memory.block_lookup memory root old stored)

/-- Actual initialization transfers arbitrary payload weight into the heap.
    Empty-cell checking is recovered from the accepted memory operation. -/
theorem write_payload_tally (weight : Core.Value → Nat) (memory : Memory.StateOf Core.Value)
    (access : Memory.Access) (index : Nat) (value : Core.Value)
    (accepted : (Memory.writeEmpty memory access index value).2 = .ok ()) :
    heapPayloadTally weight (Memory.writeEmpty memory access index value).1 =
      heapPayloadTally weight memory + weight value := by
  let next := (Memory.writeEmpty memory access index value).1
  have written : Memory.writeEmpty memory access index value = (next, .ok ()) := Prod.ext rfl accepted
  obtain ⟨root, block, _permission, stored, empty, _pushed, effect⟩ :=
    memory_write_buffer_push memory next access index value written
  have outer := replace_tally weight memory root block ⟨block.slots.set index (some value)⟩ stored
  have inner := payload_tally_set weight block.slots index none (some value) empty
  simp only [Option.elim_some, Option.elim_none, Nat.add_zero] at inner
  change heapPayloadTally weight next = _
  rw [effect]
  dsimp only [Storage.Block.slots_mk] at outer
  omega

/-- Actual take conserves arbitrary payload weight between heap and result. -/
theorem take_payload_tally (weight : Core.Value → Nat) (memory : Memory.StateOf Core.Value)
    (access : Memory.Access) (index : Nat) (value : Core.Value)
    (accepted : (Memory.take memory access index).2 = .ok value) :
    heapPayloadTally weight (Memory.take memory access index).1 + weight value = heapPayloadTally weight memory := by
  let next := (Memory.take memory access index).1
  have taken : Memory.take memory access index = (next, .ok value) := Prod.ext rfl accepted
  obtain ⟨root, block, _permission, stored, initialized, effect⟩ :=
    Memory.take_cell_effect memory next access index value taken
  have outer := replace_tally weight memory root block ⟨block.slots.set index none⟩ stored
  have inner := payload_tally_set weight block.slots index (some value) none initialized
  simp only [Option.elim_some, Option.elim_none, Nat.add_zero] at inner
  change heapPayloadTally weight next + weight value = _
  rw [effect]
  dsimp only [Storage.Block.slots_mk] at outer
  omega

/-- Terminal disposal cannot increase any nonnegative heap payload weight. -/
theorem dispose_payload_tally_le (weight : Core.Value → Nat) (memory : Memory.StateOf Core.Value) (value : Core.Value) :
    heapPayloadTally weight (disposeHeapValue memory value).1 ≤ heapPayloadTally weight memory := by
  cases value with
  | owner root =>
    cases answer : (Memory.releaseEmpty memory root).2 with
    | error reason =>
      change heapPayloadTally weight (Memory.releaseEmpty memory root).1 ≤ _
      rw [Memory.release_empty_refused memory root reason answer]
      exact Nat.le_refl _
    | ok doneValue =>
      cases doneValue
      obtain ⟨block, stored, _free, _empty, effect⟩ := Memory.release_empty_safe memory root answer
      have outer := sum_set_weight memory.blocks root (some block) none
        (fun cell => (cell.map (fun block => payloadTally weight block.slots)).getD 0)
        (Memory.block_lookup memory root block stored)
      change heapPayloadTally weight (Memory.releaseEmpty memory root).1 ≤ _
      rw [effect]
      change heapPayloadTally weight { memory with blocks := memory.blocks.set root none } ≤ heapPayloadTally weight memory
      change heapPayloadTally weight { memory with blocks := memory.blocks.set root none } +
        payloadTally weight block.slots = heapPayloadTally weight memory + 0 at outer
      omega
  | reference identity =>
    change heapPayloadTally weight (Memory.endLoan memory identity).1 ≤ _
    simp only [heapPayloadTally, Memory.end_loan_blocks, Nat.le_refl]
  | number value => exact Nat.le_refl _
  | boolean value => exact Nat.le_refl _
  | capability token | scalarAggregate token => exact Nat.le_refl _

end Witchy.Storage

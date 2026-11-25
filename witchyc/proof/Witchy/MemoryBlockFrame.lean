import Witchy.MemoryContents

namespace Witchy.Memory

/-- A content frame for all allocations outside a finite write footprint.
    Loan/allocator metadata and ownership permissions are separate assertions. -/
def BlockFrame (before after : StateOf Cell) (written : List Handle) : Prop :=
  ∀ handle, handle ∉ written → block? after handle = block? before handle

theorem BlockFrame.refl (memory : StateOf Cell) (written : List Handle) : BlockFrame memory memory written := by
  intro handle _
  rfl

theorem BlockFrame.trans (before middle after : StateOf Cell) (written : List Handle)
    (first : BlockFrame before middle written) (second : BlockFrame middle after written) : BlockFrame before after written := by
  intro handle outside
  exact (second handle outside).trans (first handle outside)

theorem BlockFrame.of_blocks (before after : StateOf Cell) (written : List Handle) (same : after.blocks = before.blocks) :
    BlockFrame before after written := by
  intro handle _
  simp only [block?, same]

theorem end_loan_blocks (memory : StateOf Cell) (identity : LoanId) : (endLoan memory identity).1.blocks = memory.blocks := by
  unfold endLoan
  split <;> rfl

theorem allocate_block_frame (memory : StateOf Cell) (count : Nat) :
    BlockFrame memory (allocate memory count).1 [memory.blocks.length] := by
  intro handle outside
  have different : handle ≠ memory.blocks.length := by simpa using outside
  unfold allocate
  split <;> try rfl
  split <;> try rfl
  simp only [block?]
  by_cases old : handle < memory.blocks.length
  · rw [List.getElem?_append_left old]
  · have beyond : memory.blocks.length < handle := Nat.lt_of_le_of_ne (Nat.le_of_not_gt old) different.symm
    rw [List.getElem?_append_right (by omega)]
    have oldNone : memory.blocks[handle]? = none := List.getElem?_eq_none (by omega)
    have tailNone : ([some (⟨List.replicate count none⟩ : Storage.Block Cell)])[handle - memory.blocks.length]? = none := by
      apply List.getElem?_eq_none
      simp only [List.length_singleton]
      omega
    simp only [tailNone, oldNone]

end Witchy.Memory

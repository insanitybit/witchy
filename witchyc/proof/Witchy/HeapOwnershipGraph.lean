import Witchy.HeapResources
import Witchy.GenericMemoryRelease

namespace Witchy.Storage

/-- A child allocation is owned by a payload in the parent's actual storage.
    The direction permits structural recursion from a parent into its children. -/
def OwnsChild (memory : Memory.StateOf Core.Value) (child parent : Memory.Handle) : Prop :=
  ∃ block, Memory.block? memory parent = some block ∧ some (.owner child) ∈ block.slots

/-- Inventory equality alone does not establish termination of nested cleanup.
    This is a separate requirement, not a consequence of resource counting. -/
def OwnershipWellFounded (memory : Memory.StateOf Core.Value) : Prop :=
  WellFounded (OwnsChild memory)

theorem ownership_blocks_frame (before after : Memory.StateOf Core.Value)
    (same : after.blocks = before.blocks) : OwnsChild after = OwnsChild before := by
  funext child parent
  simp only [OwnsChild, Memory.block?, same]

theorem ownership_subrelation (before after : Memory.StateOf Core.Value)
    (wellFounded : OwnershipWellFounded before)
    (removedOnly : ∀ child parent, OwnsChild after child parent → OwnsChild before child parent) :
    OwnershipWellFounded after :=
  Subrelation.wf (fun edge => removedOnly _ _ edge) wellFounded

theorem initial_ownership_wellFounded (limits : Memory.Limits) :
    OwnershipWellFounded (Memory.initialOf Core.Value limits) := by
  refine ⟨fun parent => Acc.intro parent ?_⟩
  intro child edge
  obtain ⟨block, found, _⟩ := edge
  simp [Memory.block?, Memory.initialOf] at found

theorem ownership_no_self (memory : Memory.StateOf Core.Value)
    (wellFounded : OwnershipWellFounded memory) (handle : Memory.Handle) :
    ¬ OwnsChild memory handle handle := by
  have accessible := wellFounded.apply handle
  induction accessible with
  | intro parent _ ih =>
    intro edge
    exact ih parent edge edge

/-- Actual borrowing only changes loan metadata, including on refusal. -/
theorem borrow_ownership_graph (memory : Memory.StateOf Core.Value) (handle : Memory.Handle)
    (exclusive : Bool) :
    OwnsChild (Memory.borrow memory handle exclusive).1 = OwnsChild memory := by
  apply ownership_blocks_frame
  unfold Memory.borrow
  split
  · rfl
  · split
    · split <;> rfl
    · rfl

theorem end_ownership_graph (memory : Memory.StateOf Core.Value) (identity : Memory.LoanId) :
    OwnsChild (Memory.endLoan memory identity).1 = OwnsChild memory := by
  apply ownership_blocks_frame
  unfold Memory.endLoan
  split <;> rfl

theorem borrow_ownership_wellFounded (memory : Memory.StateOf Core.Value)
    (handle : Memory.Handle) (exclusive : Bool) (valid : OwnershipWellFounded memory) :
    OwnershipWellFounded (Memory.borrow memory handle exclusive).1 := by
  unfold OwnershipWellFounded
  rw [borrow_ownership_graph]
  exact valid

theorem end_ownership_wellFounded (memory : Memory.StateOf Core.Value)
    (identity : Memory.LoanId) (valid : OwnershipWellFounded memory) :
    OwnershipWellFounded (Memory.endLoan memory identity).1 := by
  unfold OwnershipWellFounded
  rw [end_ownership_graph]
  exact valid

/-- Checked empty release removes storage and cannot introduce an ownership edge. -/
theorem release_ownership_wellFounded (memory : Memory.StateOf Core.Value)
    (handle : Memory.Handle) (valid : OwnershipWellFounded memory)
    (accepted : (Memory.releaseEmpty memory handle).2 = .ok ()) :
    OwnershipWellFounded (Memory.releaseEmpty memory handle).1 := by
  obtain ⟨block, _, _, _, effect⟩ := Memory.release_empty_safe memory handle accepted
  apply ownership_subrelation memory _ valid
  intro child parent edge
  obtain ⟨stored, found, member⟩ := edge
  rw [effect] at found
  by_cases same : handle = parent
  · subst parent
    simp only [Memory.block?, List.getElem?_set_self'] at found
    cases lookup : memory.blocks[handle]? <;> simp [lookup, Function.const] at found
  · refine ⟨stored, ?_, member⟩
    simpa only [Memory.block?, List.getElem?_set_ne same] using found

/-- A single self-owning allocation passes inventory accounting. It cannot
    satisfy the termination requirement, and must never be admitted as safe. -/
theorem inventory_does_not_imply_wellFounded :
    ∃ memory : Memory.StateOf Core.Value,
      OwnsHeapResources [] memory ∧ ¬ OwnershipWellFounded memory := by
  let memory : Memory.StateOf Core.Value :=
    { blocks := [some ⟨[some (.owner 0)]⟩], loans := [], available := 0,
      limits := ⟨1, 1, 0⟩, nextLoan := 0 }
  refine ⟨memory, ?_, ?_⟩
  · intro resource
    cases resource with
    | allocation handle =>
      cases handle <;>
        simp [memory, Core.registerCount, heapResources, blockResources,
          Core.weight, Core.valueResource, Core.memoryCount, Memory.block?]
    | loan identity =>
      simp [memory, Core.registerCount, heapResources, blockResources,
        Core.weight, Core.valueResource, Core.memoryCount, Memory.loan?]
  · intro valid
    apply ownership_no_self memory valid 0
    exact ⟨⟨[some (.owner 0)]⟩, rfl, by simp⟩

end Witchy.Storage

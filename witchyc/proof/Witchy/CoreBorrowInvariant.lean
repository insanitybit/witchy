import Witchy.CoreBorrowEntry
import Witchy.CoreEndLoan
import Witchy.CoreMemoryEvidence

namespace Witchy.Core

private theorem borrowed_put_invariant (s next : StateOf Value) (dst source handle identity : Nat) (exclusive : Bool)
    (known : StorageStateInvariant s) (valid : Memory.LoansValid s.memory)
    (inserted : put { s with memory := (Memory.borrow s.memory handle exclusive).1 } dst (.reference identity) = .ok next)
    (empty : s.registers[dst]? = some none)
    (borrowed : (Memory.borrow s.memory handle exclusive).2 = .ok identity)
    (performed : step s (.borrow dst source exclusive) = .ok next) : StorageStateInvariant next := by
  have roots := step_loan_roots s next (.borrow dst source exclusive) known.roots performed
  obtain ⟨_, shape⟩ := put_shape _ _ _ _ inserted
  rw [shape] at roots ⊢
  have blocks : (Memory.borrow s.memory handle exclusive).1.blocks = s.memory.blocks := by
    unfold Memory.borrow
    repeat' first | rfl | split
  refine ⟨Storage.borrow_to_register_owns s.registers s.memory handle identity dst exclusive
    known.inventory valid empty borrowed,
    Storage.borrow_ownership_wellFounded s.memory handle exclusive known.structural, ?_, roots⟩
  constructor
  · exact Storage.cells_caps_set s.authority s.registers dst _ known.capabilities.1
      (by intro value same; cases same; trivial)
  · simpa only [Storage.heapCapabilitiesValid, blocks] using known.capabilities.2

/-- Both shared and exclusive accepted borrows preserve storage. The existing
    metadata invariant supplies freshness of the issued loan identity. -/
theorem borrow_preserves_storage (s next : StateOf Value) (dst source : Nat) (exclusive : Bool)
    (known : StorageStateInvariant s) (valid : Memory.LoansValid s.memory)
    (accepted : step s (.borrow dst source exclusive) = .ok next) : StorageStateInvariant next := by
  have original := accepted
  simp only [step, Bind.bind, fromMemory] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    exact borrowed_put_invariant s next dst source _ _ exclusive known valid accepted
      (vacant_register_lookup s dst (by assumption)) (from_memory_ok _ _ (by assumption)) original

theorem end_loan_preserves_storage (s next : StateOf Value) (source : Nat)
    (known : StorageStateInvariant s) (valid : Memory.LoansValid s.memory)
    (accepted : step s (.endLoan source) = .ok next) : StorageStateInvariant next := by
  have original := accepted
  simp only [step, Bind.bind, fromMemory] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    obtain ⟨produced, performed, invariant, _⟩ := end_loan_storage_ready s source _
      (get_register_lookup s source _ (by assumption)) known valid
    have same := original.symm.trans performed
    cases same
    exact invariant

end Witchy.Core

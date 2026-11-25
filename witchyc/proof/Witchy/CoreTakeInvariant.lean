import Witchy.CoreValueTransferContracts
import Witchy.CoreBorrowRoots
import Witchy.StorageStateInvariant
import Witchy.CoreMemoryEvidence

namespace Witchy.Core

theorem take_move_storage_ready (s : StateOf Value) (dst src index : Nat)
    (value : Value) (permission : Memory.Access) (offset : UInt64)
    (known : StorageStateInvariant s) (empty : s.registers[dst]? = some none)
    (accessed : access s src = .ok permission) (position : number s index = .ok offset)
    (taken : (Memory.take s.memory permission offset.toNat).2 = .ok value)
    (unborrowed : TransferUnborrowed (Memory.take s.memory permission offset.toNat).1 value) :
    ∃ next, step s (.takeMove dst src index) = .ok next ∧ StorageStateInvariant next := by
  obtain ⟨next, performed, inventory, structural, capabilities, _⟩ :=
    take_move_contract s dst src index offset permission value empty accessed position taken unborrowed
      known.inventory known.structural known.capabilities
  exact ⟨next, performed, inventory, structural, capabilities,
    step_loan_roots s next (.takeMove dst src index) known.roots performed⟩

/-- The consuming extractor's actual vacancy and owner-loan guards suffice for
    storage preservation. No initialized-prefix or concrete payload type premise. -/
theorem take_move_preserves_storage (s next : StateOf Value) (dst src index : Nat)
    (known : StorageStateInvariant s) (accepted : step s (.takeMove dst src index) = .ok next) :
    StorageStateInvariant next := by
  have original := accepted
  simp only [step, StorageCell.ownedCells, StorageCell.decodeValue, ↓reduceIte,
    Bind.bind, fromMemory] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    obtain ⟨produced, performed, invariant⟩ := take_move_storage_ready s dst src index
      _ _ _ known (vacant_register_lookup s dst (by assumption)) (by assumption) (by assumption)
      (from_memory_ok _ _ (by assumption)) (by
        unfold TransferUnborrowed
        split <;> simp_all)
    have same := original.symm.trans performed
    cases same
    exact invariant

end Witchy.Core

import Witchy.CoreValueTransferContracts
import Witchy.CoreBorrowRoots
import Witchy.StorageStateInvariant
import Witchy.CoreMemoryEvidence

namespace Witchy.Core

/-- The receiving owner is derived from an actual resolved access, including
    loan-token access. A caller does not supply a second root registration. -/
theorem store_move_storage_ready (s : StateOf Value) (dst index source : Nat)
    (value : Value) (permission : Memory.Access) (offset : UInt64) (root : Nat)
    (known : StorageStateInvariant s) (present : get s source = .ok value)
    (accessed : access s dst = .ok permission) (position : number s index = .ok offset)
    (resolved : Memory.resolve s.memory permission true = .ok root)
    (unborrowed : TransferUnborrowed s.memory value) (different : value ≠ .owner root)
    (written : (Memory.writeEmpty s.memory permission offset.toNat value).2 = .ok ()) :
    ∃ next, step s (.storeMove dst index source) = .ok next ∧ StorageStateInvariant next := by
  obtain ⟨receiver, owner⟩ := resolved_access_root s dst root permission known.roots accessed resolved
  obtain ⟨next, performed, inventory, structural, capabilities, _⟩ :=
    store_move_root_contract s dst index source receiver root offset permission value
      (get_register_lookup s source value present) owner accessed position resolved unborrowed different
      known.inventory known.structural known.capabilities written
  exact ⟨next, performed, inventory, structural, capabilities,
    step_loan_roots s next (.storeMove dst index source) known.roots performed⟩

/-- Successful Core stores preserve the whole storage invariant. All readiness
    conditions above are recovered from the actual accepted instruction. -/
theorem store_move_preserves_storage (s next : StateOf Value) (dst index source : Nat)
    (known : StorageStateInvariant s) (accepted : step s (.storeMove dst index source) = .ok next) :
    StorageStateInvariant next := by
  have original := accepted
  simp only [step, StorageCell.ownedCells, StorageCell.encodeValue, ↓reduceIte,
    Bind.bind, Pure.pure, fromMemory] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    obtain ⟨produced, performed, invariant⟩ := store_move_storage_ready s dst index source
      _ _ _ _ known (by assumption) (by assumption) (by assumption)
      (from_memory_ok _ _ (by assumption)) (by
        unfold TransferUnborrowed
        split <;> simp_all) (by
        intro same
        subst_vars
        simp_all) (from_memory_ok _ _ (by assumption))
    have same := original.symm.trans performed
    cases same
    exact invariant

end Witchy.Core

import Witchy.CoreTrivialRelease
import Witchy.HeapOwnershipGraph
import Witchy.HeapCapabilityTransfer
import Witchy.CoreBorrowRoots
import Witchy.StorageStateInvariant
import Witchy.CoreMemoryEvidence

namespace Witchy.Core

private theorem released_storage_graph (memory : Memory.StateOf Value) (handle : Nat)
    (known : Storage.OwnershipWellFounded memory)
    (accepted : (Memory.releaseEmpty memory handle Value.trivialRelease).2 = .ok ()) :
    Storage.OwnershipWellFounded (Memory.releaseEmpty memory handle Value.trivialRelease).1 := by
  obtain ⟨block, _, _, _, effect⟩ := Memory.release_checked_safe memory handle Value.trivialRelease accepted
  apply Storage.ownership_subrelation memory _ known
  intro child parent edge
  obtain ⟨stored, found, member⟩ := edge
  rw [effect] at found
  by_cases same : handle = parent
  · subst parent
    simp only [Memory.block?, List.getElem?_set_self'] at found
    cases lookup : memory.blocks[handle]? <;> simp [lookup, Function.const] at found
  · exact ⟨stored, by simpa only [Memory.block?, List.getElem?_set_ne same] using found, member⟩

/-- Actual Core release can discard initialized trivial scalars, so its
    preservation theorem uses the checked-disposition inventory theorem. -/
theorem release_preserves_storage (s next : StateOf Value) (source : Nat)
    (known : StorageStateInvariant s) (accepted : step s (.release source) = .ok next) :
    StorageStateInvariant next := by
  have roots := step_loan_roots s next (.release source) known.roots accepted
  simp only [step, StorageCell.release, Bind.bind, Pure.pure, fromMemory] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    have released := from_memory_ok _ _ (by assumption)
    obtain ⟨block, _, _, _, effect⟩ := Memory.release_checked_safe _ _ Value.trivialRelease released
    refine ⟨Storage.core_trivial_release_owned s.registers s.memory source _ known.inventory
      (get_register_lookup s source _ (by assumption)) released,
      released_storage_graph s.memory _ known.structural released, ⟨?_, ?_⟩, roots⟩
    · exact Storage.cells_caps_set s.authority s.registers source none known.capabilities.1
        (by intro value impossible; cases impossible)
    · rw [effect]
      exact Storage.release_heap_capabilities s.authority s.memory _ known.capabilities.2

end Witchy.Core

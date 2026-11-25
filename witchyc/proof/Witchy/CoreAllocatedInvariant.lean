import Witchy.CoreAllocationInvariant
import Witchy.CoreMemoryEvidence

namespace Witchy.Core

theorem allocated_put_invariant (s next : StateOf Value) (dst count handle : Nat)
    (inserted : put { s with memory := (Memory.allocate s.memory count).1 } dst (.owner handle) = .ok next)
    (known : StorageStateInvariant s) (empty : s.registers[dst]? = some none)
    (allocated : (Memory.allocate s.memory count).2 = .ok handle) : StorageStateInvariant next := by
  have roots := put_loan_roots { s with memory := (Memory.allocate s.memory count).1 } next dst (.owner handle)
    (allocate_loan_roots s.registers s.memory count known.roots) inserted
  obtain ⟨_, shape⟩ := put_shape _ _ _ _ inserted
  rw [shape] at roots ⊢
  refine ⟨Storage.allocate_to_register_owns s.registers s.memory count handle dst known.inventory empty allocated,
    Storage.allocation_ownership_wellFounded s.memory count known.structural, ?_, roots⟩
  constructor
  · exact Storage.cells_caps_set s.authority s.registers dst _ known.capabilities.1
      (by intro value same; cases same; trivial)
  · exact Storage.allocation_heap_capabilities s.authority s.memory count known.capabilities.2

theorem allocate_preserves_storage (s next : StateOf Value) (dst count : Nat)
    (known : StorageStateInvariant s) (accepted : step s (.allocate dst count) = .ok next) :
    StorageStateInvariant next := by
  simp only [step, Bind.bind, fromMemory] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    exact allocated_put_invariant s next dst _ _ accepted known
      (vacant_register_lookup s dst (by assumption)) (from_memory_ok _ _ (by assumption))

private theorem copied_invariant (s next : StateOf Value) (dst : Nat) (value : Value)
    (accepted : putCopy s dst value = .ok next) (known : StorageStateInvariant s) : StorageStateInvariant next :=
  putCopy_storage_invariant s next dst value known accepted

/-- Refusal and success preserve storage without a caller choosing the
    allocator outcome. The accepted tag/error writes are checked scalar writes. -/
theorem try_allocate_preserves_storage (s next : StateOf Value) (tag dst error count : Nat)
    (known : StorageStateInvariant s) (accepted : step s (.tryAllocate tag dst error count) = .ok next) :
    StorageStateInvariant next := by
  simp only [step, Bind.bind] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals apply copied_invariant
  all_goals first | assumption | skip
  all_goals first
    | (apply allocated_put_invariant (s := s) <;> first
        | assumption
        | exact known
        | exact vacant_register_lookup s dst (by assumption))
    | (apply copied_invariant (s := s) <;> assumption)

end Witchy.Core

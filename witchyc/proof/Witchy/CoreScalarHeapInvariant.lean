import Witchy.CoreStoreInvariant
import Witchy.CoreScalarInvariant

namespace Witchy.Core

theorem write_number_storage_invariant (s : StateOf Value) (permission : Memory.Access) (index source : Nat)
    (value : UInt64) (known : StorageStateInvariant s) (present : get s source = .ok (.number value))
    (accessed : ∃ operand, access s operand = .ok permission)
    (written : (Memory.writeEmpty s.memory permission index (.number value)).2 = .ok ()) :
    StorageStateInvariant { s with memory := (Memory.writeEmpty s.memory permission index (.number value)).1 } := by
  have complete : Memory.writeEmpty s.memory permission index (.number value) =
      ((Memory.writeEmpty s.memory permission index (.number value)).1, .ok ()) := Prod.ext rfl written
  obtain ⟨root, block, resolved, _, _, _, _⟩ := Storage.memory_write_buffer_push s.memory _ permission index (Value.number value) complete
  obtain ⟨operand, access⟩ := accessed
  obtain ⟨receiver, owner⟩ := resolved_access_root s operand root permission known.roots access resolved
  refine ⟨?_, Storage.write_root_ownership_wellFounded s.registers s.memory permission index receiver root (.number value)
    known.inventory owner resolved (by intro impossible; cases impossible) known.structural written,
    ⟨known.capabilities.1, ?_⟩,
    write_loan_roots s.registers s.memory permission index (.number value) known.roots⟩
  · intro resource
    obtain ⟨stored, live⟩ := Storage.write_heap_inventory s.memory permission index (.number value) written resource
    change registerCount s.registers resource + _ = _
    rw [stored, show weight resource (some (.number value)) = 0 from rfl, Nat.add_zero, live]
    exact known.inventory resource
  · exact (Storage.write_from_register_capabilities s.authority s.registers s.memory permission index source (.number value)
      known.capabilities (get_register_lookup s source (.number value) present) written).2

theorem write_empty_preserves_storage (s next : StateOf Value) (dst index source : Nat)
    (known : StorageStateInvariant s) (accepted : step s (.writeEmpty dst index source) = .ok next) :
    StorageStateInvariant next := by
  simp only [step, Bind.bind, Pure.pure, fromMemory, StorageCell.encodeNumber] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    exact write_number_storage_invariant s _ _ source _ known
      (number_get_value s source _ (by assumption)) ⟨dst, by assumption⟩
      (from_memory_ok _ _ (by assumption))

theorem taken_number_storage_invariant (s : StateOf Value) (permission : Memory.Access) (index : Nat)
    (value : UInt64) (known : StorageStateInvariant s)
    (taken : (Memory.take s.memory permission index).2 = .ok (.number value)) :
    StorageStateInvariant { s with memory := (Memory.take s.memory permission index).1 } := by
  refine ⟨?_, Storage.take_ownership_wellFounded s.memory permission index (.number value) known.structural taken,
    ⟨known.capabilities.1, ?_⟩, take_loan_roots s.registers s.memory permission index known.roots⟩
  · intro resource
    obtain ⟨stored, live⟩ := Storage.take_heap_inventory s.memory permission index (.number value) taken resource
    have zero : weight resource (some (.number value)) = 0 := rfl
    rw [zero, Nat.add_zero] at stored
    change registerCount s.registers resource + _ = _
    rw [stored, live]
    exact known.inventory resource
  · exact (Storage.take_to_register_capabilities s.authority s.registers s.memory permission index 0 (.number value)
      known.capabilities taken).2

private theorem decoded_number (value : Value) (number : UInt64)
    (accepted : StorageCell.decodeNumber value = .ok number) : value = .number number := by
  cases value <;> simp_all [StorageCell.decodeNumber]

theorem take_preserves_storage (s next : StateOf Value) (dst src index : Nat)
    (known : StorageStateInvariant s) (accepted : step s (.take dst src index) = .ok next) :
    StorageStateInvariant next := by
  simp only [step, Bind.bind, fromMemory, StorageCell.decodeNumber] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals have decoded := decoded_number _ _ (by assumption)
  all_goals subst_vars
  all_goals
    apply putCopy_storage_invariant _ _ _ _ ?_ accepted
    exact taken_number_storage_invariant s _ _ _ known (from_memory_ok _ _ (by assumption))

end Witchy.Core

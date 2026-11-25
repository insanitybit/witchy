import Witchy.CoreValueTransfer
import Witchy.HeapOwnershipTransfer
import Witchy.HeapCapabilityTransfer

namespace Witchy.Core

/-- A consuming store into a registered root preserves resource inventory,
    structural ownership and capability validity through the actual Core step.
    General reference lifetimes and location-sensitive relocation are separate. -/
theorem store_move_root_contract (s : StateOf Value) (dst index source receiver root : Nat)
    (offset : UInt64) (permission : Memory.Access) (value : Value)
    (present : s.registers[source]? = some (some value))
    (rootOwner : s.registers[receiver]? = some (some (.owner root)))
    (accessed : access s dst = .ok permission) (position : number s index = .ok offset)
    (resolved : Memory.resolve s.memory permission true = .ok root)
    (unborrowed : TransferUnborrowed s.memory value) (different : value ≠ .owner root)
    (owned : Storage.OwnsHeapResources s.registers s.memory)
    (structural : Storage.OwnershipWellFounded s.memory)
    (capabilities : Storage.allCapabilitiesValid s.authority s.registers s.memory)
    (accepted : (Memory.writeEmpty s.memory permission offset.toNat value).2 = .ok ()) :
    ∃ next, step s (.storeMove dst index source) = .ok next ∧
      Storage.OwnsHeapResources next.registers next.memory ∧
      Storage.OwnershipWellFounded next.memory ∧
      Storage.allCapabilitiesValid next.authority next.registers next.memory ∧
      next.authority = s.authority := by
  have avoids : TransferNotSelf root value := by cases value <;> simp_all [TransferNotSelf]
  have effect := store_move_effect s dst index source offset permission root value
    present accessed position resolved unborrowed avoids accepted
  obtain ⟨inventory, graph⟩ := Storage.write_register_root_contract s.registers s.memory permission
    offset.toNat source receiver root value owned structural present rootOwner resolved different accepted
  have caps := Storage.write_from_register_capabilities s.authority s.registers s.memory
    permission offset.toNat source value capabilities present accepted
  exact ⟨_, effect, inventory, graph, caps, rfl⟩

/-- Actual extraction to a vacant register preserves all three independent
    components, including when the extracted value owns another allocation. -/
theorem take_move_contract (s : StateOf Value) (dst src index : Nat) (offset : UInt64)
    (permission : Memory.Access) (value : Value)
    (empty : s.registers[dst]? = some none)
    (accessed : access s src = .ok permission) (position : number s index = .ok offset)
    (accepted : (Memory.take s.memory permission offset.toNat).2 = .ok value)
    (unborrowed : TransferUnborrowed (Memory.take s.memory permission offset.toNat).1 value)
    (owned : Storage.OwnsHeapResources s.registers s.memory)
    (structural : Storage.OwnershipWellFounded s.memory)
    (capabilities : Storage.allCapabilitiesValid s.authority s.registers s.memory) :
    ∃ next, step s (.takeMove dst src index) = .ok next ∧
      Storage.OwnsHeapResources next.registers next.memory ∧
      Storage.OwnershipWellFounded next.memory ∧
      Storage.allCapabilitiesValid next.authority next.registers next.memory ∧
      next.authority = s.authority := by
  have effect := take_move_effect s dst src index offset permission value empty accessed position accepted unborrowed
  obtain ⟨inventory, graph⟩ := Storage.take_register_ownership_contract s.registers s.memory permission
    offset.toNat dst value owned structural empty accepted
  have caps := Storage.take_to_register_capabilities s.authority s.registers s.memory
    permission offset.toNat dst value capabilities accepted
  exact ⟨_, effect, inventory, graph, caps, rfl⟩

end Witchy.Core

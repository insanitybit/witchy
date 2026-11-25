import Witchy.CleanupOwnerExpansion
import Witchy.CoreValueTransferContracts

namespace Witchy.Storage

/-- Incoming initialization, root permission and child owner-loan eligibility
    derive expansion success. The source cell is cleared exactly once. -/
theorem expand_cleanup_owner_ready (memory : Memory.StateOf Core.Value) (root index : Nat)
    (block : Block Core.Value) (child : Core.Value) (tail : List Core.Value)
    (stored : Memory.block? memory root = some block)
    (free : (Memory.active memory root).isEmpty = true)
    (initialized : block.slots[index]? = some (some child))
    (eligible : Core.TransferUnborrowed memory child) :
    expandCleanupOwner memory root index tail =
      ({ memory with blocks := memory.blocks.set root (some ⟨block.slots.set index none⟩) },
        .ok (), child :: .owner root :: tail) := by
  have permission : Memory.resolve memory (.owner root) true = .ok root := by simp [Memory.resolve, free]
  have taken : Memory.take memory (.owner root) index =
      ({ memory with blocks := memory.blocks.set root (some ⟨block.slots.set index none⟩) }, .ok child) := by
    simp [Memory.take, permission, stored, initialized]
  simp only [expandCleanupOwner, taken]
  cases child <;> simp_all [cleanupPayloadUnborrowed, Core.TransferUnborrowed, Memory.active]

/-- Borrowing a child prevents extraction into pending cleanup. The unpublished
    take is not committed; the original parent cell and worklist survive. -/
theorem expand_cleanup_owner_borrowed_child (memory : Memory.StateOf Core.Value) (root index child : Nat)
    (block : Block Core.Value) (tail : List Core.Value)
    (stored : Memory.block? memory root = some block)
    (free : (Memory.active memory root).isEmpty = true)
    (initialized : block.slots[index]? = some (some (.owner child)))
    (borrowed : (Memory.active memory child).isEmpty = false) :
    expandCleanupOwner memory root index tail = (memory, .error .borrowed, .owner root :: tail) := by
  have permission : Memory.resolve memory (.owner root) true = .ok root := by simp [Memory.resolve, free]
  have taken : Memory.take memory (.owner root) index =
      ({ memory with blocks := memory.blocks.set root (some ⟨block.slots.set index none⟩) }, .ok (.owner child)) := by
    simp [Memory.take, permission, stored, initialized]
  have blocked : (memory.loans.filter (fun loan => loan.target == child)).isEmpty = false := borrowed
  simp [expandCleanupOwner, taken, cleanupPayloadUnborrowed, Memory.active, blocked]

end Witchy.Storage

namespace Witchy.Core

/-- Expansion's memory transfer is an actual consuming Core step. Pending-list
    scheduling is abstract caller bookkeeping; a complete recursive source loop
    and location-sensitive relocation remain separate obligations. -/
theorem cleanup_owner_expansion_core (s : StateOf Value) (root dst src index : Nat) (offset : UInt64)
    (block : Storage.Block Value) (child : Value) (tail : List Value)
    (owner : s.registers[src]? = some (some (.owner root)))
    (empty : s.registers[dst]? = some none) (position : number s index = .ok offset)
    (stored : Memory.block? s.memory root = some block)
    (free : (Memory.active s.memory root).isEmpty = true)
    (initialized : block.slots[offset.toNat]? = some (some child))
    (eligible : TransferUnborrowed s.memory child)
    (owned : Storage.OwnsHeapResources s.registers s.memory)
    (structural : Storage.OwnershipWellFounded s.memory)
    (capabilities : Storage.allCapabilitiesValid s.authority s.registers s.memory) :
    ∃ next, step s (.takeMove dst src index) = .ok next ∧
      Storage.expandCleanupOwner s.memory root offset.toNat tail =
        (next.memory, .ok (), child :: .owner root :: tail) ∧
      next.registers = s.registers.set dst (some child) ∧
      Storage.OwnsHeapResources next.registers next.memory ∧
      Storage.OwnershipWellFounded next.memory ∧
      Storage.allCapabilitiesValid next.authority next.registers next.memory ∧ next.authority = s.authority := by
  have accessed : access s src = .ok (.owner root) := by simp [access, get, owner, Bind.bind, Pure.pure]
  have permission : Memory.resolve s.memory (.owner root) true = .ok root := by simp [Memory.resolve, free]
  have taken : Memory.take s.memory (.owner root) offset.toNat =
      ({ s.memory with blocks := s.memory.blocks.set root (some ⟨block.slots.set offset.toNat none⟩) }, .ok child) := by
    simp [Memory.take, permission, stored, initialized]
  have accepted : (Memory.take s.memory (.owner root) offset.toNat).2 = .ok child := by rw [taken]
  have afterEligible : TransferUnborrowed (Memory.take s.memory (.owner root) offset.toNat).1 child := by
    cases child <;> simp_all [TransferUnborrowed, Memory.active]
  obtain ⟨next, performed, inventory, graph, caps, authority⟩ :=
    take_move_contract s dst src index offset (.owner root) child empty accessed position accepted afterEligible
      owned structural capabilities
  have effect := take_move_effect s dst src index offset (.owner root) child empty accessed position accepted afterEligible
  have same := performed.symm.trans effect
  cases same
  refine ⟨_, performed, ?_, rfl, inventory, graph, caps, authority⟩
  rw [taken]
  exact Storage.expand_cleanup_owner_ready s.memory root offset.toNat block child tail stored free initialized eligible

end Witchy.Core

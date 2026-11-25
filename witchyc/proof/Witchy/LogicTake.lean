import Witchy.LogicFootprint
import Witchy.CoreValueTransfer
import Witchy.MemoryContents

namespace Witchy.Logic
open Core

/-- Extraction's only writes are its destination and the selected block.
    Local assertion wrappers still have to supply these operational facts. -/
theorem take_locality (store : StateOf Value) (dst src index handle : Nat)
    (offset : UInt64) (permission : Memory.Access) (block : Storage.Block Value) (value : Value)
    (empty : store.registers[dst]? = some none)
    (accessed : access store src = .ok permission) (position : number store index = .ok offset)
    (resolved : Memory.resolve store.memory permission true = .ok handle)
    (found : Memory.block? store.memory handle = some block)
    (initialized : block.slots[offset.toNat]? = some (some value))
    (movable : TransferUnborrowed
      { store.memory with blocks := store.memory.blocks.set handle (some ⟨block.slots.set offset.toNat none⟩) } value) :
    ∃ next, Core.step store (.takeMove dst src index) = .ok next ∧
      UnchangedOutside [.register dst, .block handle] store next ∧
      next.registers[dst]? = some (some value) ∧
      Memory.block? next.memory handle = some ⟨block.slots.set offset.toNat none⟩ := by
  have memoryStep : Memory.take store.memory permission offset.toNat =
      ({ store.memory with blocks := store.memory.blocks.set handle (some ⟨block.slots.set offset.toNat none⟩) }, .ok value) := by
    simp [Memory.take, resolved, found, initialized]
  have accepted := take_move_effect store dst src index offset permission value
    empty accessed position (by rw [memoryStep]) (by simpa only [memoryStep] using movable)
  refine ⟨_, accepted, ?_, ?_, ?_⟩
  · simpa only [memoryStep] using patch_register_block store dst handle
      (some value) ⟨block.slots.set offset.toNat none⟩ (by intro absent; rw [found] at absent; cases absent)
  · exact List.getElem?_set_self (List.getElem?_eq_some_iff.mp empty).1
  · simpa only [memoryStep] using Memory.block_after_set store.memory handle block
      ⟨block.slots.set offset.toNat none⟩ found

end Witchy.Logic

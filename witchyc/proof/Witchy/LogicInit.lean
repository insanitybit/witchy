import Witchy.LogicFootprint
import Witchy.CoreValueTransfer
import Witchy.MemoryContents

namespace Witchy.Logic
open Core

/-- Initialization transfers the payload and changes no observation outside
    its source register and selected block. It never overwrites a live cell. -/
theorem init_locality (store : StateOf Value) (dst index source handle : Nat)
    (offset : UInt64) (permission : Memory.Access) (block : Storage.Block Value) (value : Value)
    (present : store.registers[source]? = some (some value))
    (accessed : access store dst = .ok permission) (position : number store index = .ok offset)
    (resolved : Memory.resolve store.memory permission true = .ok handle)
    (found : Memory.block? store.memory handle = some block)
    (vacant : block.slots[offset.toNat]? = some none)
    (movable : TransferUnborrowed store.memory value) (notSelf : TransferNotSelf handle value) :
    ∃ next, Core.step store (.storeMove dst index source) = .ok next ∧
      UnchangedOutside [.register source, .block handle] store next ∧
      next.registers[source]? = some none ∧
      Memory.block? next.memory handle = some ⟨block.slots.set offset.toNat (some value)⟩ := by
  have memoryStep : Memory.writeEmpty store.memory permission offset.toNat value =
      ({ store.memory with blocks := store.memory.blocks.set handle (some ⟨block.slots.set offset.toNat (some value)⟩) }, .ok ()) := by
    simp [Memory.writeEmpty, resolved, found, vacant]
  have accepted := store_move_effect store dst index source offset permission handle value
    present accessed position resolved movable notSelf (by rw [memoryStep])
  refine ⟨_, accepted, ?_, ?_, ?_⟩
  · simpa only [memoryStep] using patch_register_block store source handle
      none ⟨block.slots.set offset.toNat (some value)⟩ (by intro absent; rw [found] at absent; cases absent)
  · exact List.getElem?_set_self (List.getElem?_eq_some_iff.mp present).1
  · simpa only [memoryStep] using Memory.block_after_set store.memory handle block
      ⟨block.slots.set offset.toNat (some value)⟩ found

end Witchy.Logic

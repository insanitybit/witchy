import Witchy.StorageBufferPush
import Witchy.Memory

namespace Witchy.Storage

/-- Actual scalar initialization refines insertion into a buffer at its logical
    end. Successful execution supplies permission and an empty target cell;
    unrelated memory and metadata are preserved by the exact update. -/
theorem memory_write_buffer_push (s next : Memory.StateOf Value) (access : Memory.Access) (index : Nat) (value : Value)
    (accepted : Memory.writeEmpty s access index value = (next, .ok ())) :
    ∃ handle block, Memory.resolve s access true = .ok handle ∧ Memory.block? s handle = some block ∧
      block.slots[index]? = some none ∧
      Buffer.push ⟨block, index⟩ value = (⟨⟨block.slots.set index (some value)⟩, index + 1⟩, none) ∧
      next = { s with blocks := s.blocks.set handle (some ⟨block.slots.set index (some value)⟩) } := by
  unfold Memory.writeEmpty at accepted
  split at accepted
  · cases accepted
  · rename_i handle permission
    split at accepted
    · cases accepted
    · rename_i block found
      split at accepted
      · cases accepted
      · cases accepted
      · rename_i cell
        cases accepted
        exact ⟨handle, block, permission, found, cell, by simp [Buffer.push, cell], rfl⟩

end Witchy.Storage

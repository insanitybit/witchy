import Witchy.CoreRegisterEffects

namespace Witchy.Core

theorem read_memory_effect (s : State) (dst src index : Nat) (reader : Memory.Access)
    (offset value : UInt64) (slot : NumberSlot s dst)
    (reference : access s src = .ok reader) (position : number s index = .ok offset)
    (read : Memory.read s.memory reader offset.toNat = .ok value) :
    step s (.read dst src index) = .ok (withNumber s dst value) := by
  simp only [step, reference, position, read, Bind.bind, fromMemory]
  exact put_number_effect s dst value slot

theorem write_memory_effect (s : State) (dst index value : Nat) (writer : Memory.Access)
    (offset item : UInt64) (reference : access s dst = .ok writer)
    (position : number s index = .ok offset) (element : number s value = .ok item)
    (written : (Memory.writeEmpty s.memory writer offset.toNat item).2 = .ok ()) :
    step s (.writeEmpty dst index value) =
      .ok { s with memory := (Memory.writeEmpty s.memory writer offset.toNat item).1 } := by
  simp [step, StorageCell.encodeNumber, reference, position, element, written, Bind.bind, fromMemory, Pure.pure]

/-- Moving a scalar out of storage changes the memory initialization state and
    fills a numeric temporary; existing numeric temporaries may be reused. -/
theorem take_memory_effect (s : State) (dst src index : Nat) (reader : Memory.Access)
    (offset value : UInt64) (slot : NumberSlot s dst)
    (reference : access s src = .ok reader) (position : number s index = .ok offset)
    (taken : (Memory.take s.memory reader offset.toNat).2 = .ok value) :
    step s (.take dst src index) =
      .ok (withNumber { s with memory := (Memory.take s.memory reader offset.toNat).1 } dst value) := by
  have updatedSlot : NumberSlot { s with memory := (Memory.take s.memory reader offset.toNat).1 } dst := slot
  have stored := put_number_effect { s with memory := (Memory.take s.memory reader offset.toNat).1 } dst value updatedSlot
  rcases slot with empty | ⟨old, present⟩
  · simp only [step, empty, reference, position, taken, fromMemory, Bind.bind]
    exact stored
  · simp only [step, present, reference, position, taken, fromMemory, Bind.bind]
    exact stored

/-- The actual two core instructions implement read-then-initialize, without a
    vector-specific instruction or a runtime call to the copy proof helper. -/
theorem copy_cell_core_effect (s : State) (temporary readerReg writerReg readIndex writeIndex : Nat)
    (reader writer : Memory.Access) (offset value : UInt64) (slot : NumberSlot s temporary)
    (readerValue : access s readerReg = .ok reader) (writerValue : access s writerReg = .ok writer)
    (readPosition : number s readIndex = .ok offset) (writePosition : number s writeIndex = .ok offset)
    (separateWriter : temporary ≠ writerReg) (separateIndex : temporary ≠ writeIndex)
    (read : Memory.read s.memory reader offset.toNat = .ok value)
    (written : (Memory.writeEmpty s.memory writer offset.toNat value).2 = .ok ()) :
    ∃ middle next, step s (.read temporary readerReg readIndex) = .ok middle ∧
      step middle (.writeEmpty writerReg writeIndex temporary) = .ok next ∧
      next.memory = (Memory.writeEmpty s.memory writer offset.toNat value).1 ∧
      next.registers = (withNumber s temporary value).registers ∧ next.authority = s.authority := by
  let middle := withNumber s temporary value
  let next := { middle with memory := (Memory.writeEmpty s.memory writer offset.toNat value).1 }
  refine ⟨middle, next, read_memory_effect s temporary readerReg readIndex reader offset value slot
    readerValue readPosition read, ?_, rfl, rfl, rfl⟩
  apply write_memory_effect middle writerReg writeIndex temporary writer offset value
  · exact (with_number_other_access s temporary writerReg value separateWriter).trans writerValue
  · exact (with_number_other_number s temporary writeIndex value separateIndex).trans writePosition
  · simp only [middle, number, with_number_get s temporary value slot, Bind.bind, Pure.pure]
  · exact written

end Witchy.Core

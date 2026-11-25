import Witchy.CoreStep

namespace Witchy.Core

variable {Cell : Type}

theorem state_fields_equal (s next : StateOf Cell) (registers : s.registers = next.registers)
    (memory : s.memory = next.memory) (authority : s.authority = next.authority) : s = next := by
  cases s
  cases next
  cases registers
  cases memory
  cases authority
  rfl

/-- Both fresh and previously used numeric temporaries are writable. The latter
    matters when one instruction address executes repeatedly in a loop. -/
def NumberSlot (s : StateOf Cell) (index : Nat) : Prop :=
  s.registers[index]? = some none ∨
    ∃ old, s.registers[index]? = some (some (.number old))

def withNumber (s : StateOf Cell) (index : Nat) (value : UInt64) : StateOf Cell :=
  { s with registers := s.registers.set index (some (.number value)) }

theorem put_number_effect (s : StateOf Cell) (index : Nat) (value : UInt64) (slot : NumberSlot s index) :
    putCopy s index (.number value) = .ok (withNumber s index value) := by
  rcases slot with empty | ⟨old, present⟩
  · simp [putCopy, withNumber, empty]
  · simp [putCopy, withNumber, present]

theorem with_number_get (s : StateOf Cell) (index : Nat) (value : UInt64) (slot : NumberSlot s index) :
    get (withNumber s index value) index = .ok (.number value) := by
  have bound : index < s.registers.length := by
    rcases slot with empty | ⟨old, present⟩
    · exact (List.getElem?_eq_some_iff.mp empty).1
    · exact (List.getElem?_eq_some_iff.mp present).1
  simp [get, withNumber, List.getElem?_set_self bound]

theorem with_number_other_get (s : StateOf Cell) (changed index : Nat) (value : UInt64)
    (different : changed ≠ index) :
    get (withNumber s changed value) index = get s index := by
  simp only [get, withNumber, List.getElem?_set_ne different]

theorem with_number_other_number (s : StateOf Cell) (changed index : Nat) (value : UInt64)
    (different : changed ≠ index) :
    number (withNumber s changed value) index = number s index := by
  simp only [number, with_number_other_get s changed index value different]

theorem with_number_other_access (s : StateOf Cell) (changed index : Nat) (value : UInt64)
    (different : changed ≠ index) :
    access (withNumber s changed value) index = access s index := by
  simp only [access, with_number_other_get s changed index value different]

end Witchy.Core

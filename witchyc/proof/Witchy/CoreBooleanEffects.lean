import Witchy.CoreRegisterEffects

namespace Witchy.Core

variable {Cell : Type}

def BoolSlot (s : StateOf Cell) (index : Nat) : Prop :=
  s.registers[index]? = some none ∨
    ∃ old, s.registers[index]? = some (some (.boolean old))

def withBool (s : StateOf Cell) (index : Nat) (value : Bool) : StateOf Cell :=
  { s with registers := s.registers.set index (some (.boolean value)) }

theorem put_bool_effect (s : StateOf Cell) (index : Nat) (value : Bool) (slot : BoolSlot s index) :
    putCopy s index (.boolean value) = .ok (withBool s index value) := by
  rcases slot with empty | ⟨old, present⟩
  · simp [putCopy, withBool, empty]
  · simp [putCopy, withBool, present]

theorem with_bool_get (s : StateOf Cell) (index : Nat) (value : Bool) (slot : BoolSlot s index) :
    get (withBool s index value) index = .ok (.boolean value) := by
  have bound : index < s.registers.length := by
    rcases slot with empty | ⟨old, present⟩
    · exact (List.getElem?_eq_some_iff.mp empty).1
    · exact (List.getElem?_eq_some_iff.mp present).1
  simp [get, withBool, List.getElem?_set_self bound]

variable [StorageCell Cell]

theorem less_effect (s : StateOf Cell) (dst left right : Nat) (a b : UInt64)
    (slot : BoolSlot s dst) (leftValue : number s left = .ok a)
    (rightValue : number s right = .ok b) :
    step s (.calculate dst left right .less) = .ok (withBool s dst (decide (a < b))) := by
  simp only [step, leftValue, rightValue, arithmetic, Bind.bind]
  exact put_bool_effect s dst _ slot

end Witchy.Core

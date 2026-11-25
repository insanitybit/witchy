import Witchy.CoreMemoryEffects
import Witchy.CoreComposition
import Witchy.ExecutionPath
import Witchy.AllocationProofs
import Witchy.Resources

namespace Witchy.Core

variable {Cell : Type}

theorem number_get_value (s : StateOf Cell) (index : Nat) (value : UInt64)
    (position : number s index = .ok value) : get s index = .ok (.number value) := by
  cases found : get s index with
  | error reason => simp [number, found, Bind.bind] at position
  | ok item =>
    cases item <;> simp only [number, found, Bind.bind, Pure.pure] at position
    all_goals cases position
    rfl

theorem number_slot_from_number (s : StateOf Cell) (index : Nat) (value : UInt64)
    (position : number s index = .ok value) : NumberSlot s index := by
  exact Or.inr ⟨value, get_register_lookup s index (.number value)
    (number_get_value s index value position)⟩

variable [StorageCell Cell]

theorem calculate_number_effect (s : StateOf Cell) (dst left right : Nat) (operation : Arithmetic)
    (a b value : UInt64) (slot : NumberSlot s dst)
    (leftValue : number s left = .ok a) (rightValue : number s right = .ok b)
    (result : arithmetic operation a b = .ok (.number value)) :
    step s (.calculate dst left right operation) = .ok (withNumber s dst value) := by
  simp only [step, leftValue, rightValue, result, Bind.bind]
  exact put_number_effect s dst value slot

theorem increment_counter_effect (s : StateOf Cell) (counter one : Nat) (length capacity : UInt64)
    (counterValue : number s counter = .ok length) (oneValue : number s one = .ok 1)
    (inside : length.toNat < capacity.toNat) :
    step s (.calculate counter counter one .add) =
      .ok (withNumber s counter (UInt64.ofNat (length.toNat + 1))) := by
  exact calculate_number_effect s counter counter one .add length 1 _
    (number_slot_from_number s counter length counterValue) counterValue oneValue
    (checked_push_increment length capacity inside)

/-- Matches the emitted constant-one / checked in-place increment pair, with
    fresh or reused constant temporaries and arbitrary continuation code. -/
theorem counter_increment_path (program : Array Instruction) (pc : Nat) (s : StateOf Cell)
    (counter one : Nat) (length capacity : UInt64) (slot : NumberSlot s one)
    (counterValue : number s counter = .ok length) (separate : one ≠ counter)
    (inside : length.toNat < capacity.toNat)
    (first : program[pc]? = some (.constant one 1))
    (second : program[pc + 1]? = some (.calculate counter counter one .add)) :
    ∃ next, ExecutionPath program 2 pc s (pc + 2) next ∧
      number next counter = .ok (UInt64.ofNat (length.toNat + 1)) ∧
      next.memory = s.memory ∧ next.authority = s.authority ∧
      next.registers = (withNumber (withNumber s one 1) counter (UInt64.ofNat (length.toNat + 1))).registers := by
  let middle := withNumber s one 1
  let value := UInt64.ofNat (length.toNat + 1)
  let next := withNumber middle counter value
  have constant : step s (.constant one 1) = .ok middle := put_number_effect s one 1 slot
  have retained : number middle counter = .ok length :=
    (with_number_other_number s one counter 1 separate).trans counterValue
  have oneValue : number middle one = .ok 1 := by
    simp only [middle, number, with_number_get s one 1 slot, Bind.bind, Pure.pure]
  have increment := increment_counter_effect middle counter one length capacity retained oneValue inside
  refine ⟨next, ?_, ?_, rfl, rfl, rfl⟩
  · exact .advance (.instruction first constant)
      (.advance (.instruction second increment) (.refl _ _))
  · simp only [next, number, with_number_get middle counter value
      (number_slot_from_number middle counter length retained), Bind.bind, Pure.pure]
    rfl

theorem counter_increment_continues (program : Array Instruction) (pc : Nat) (s : StateOf Cell)
    (counter one : Nat) (length capacity : UInt64) (slot : NumberSlot s one)
    (counterValue : number s counter = .ok length) (separate : one ≠ counter)
    (inside : length.toNat < capacity.toNat)
    (first : program[pc]? = some (.constant one 1))
    (second : program[pc + 1]? = some (.calculate counter counter one .add)) :
    ∃ next, Continues program 2 pc s (pc + 2) next ∧
      number next counter = .ok (UInt64.ofNat (length.toNat + 1)) ∧
      next.memory = s.memory ∧ next.authority = s.authority ∧
      next.registers = (withNumber (withNumber s one 1) counter (UInt64.ofNat (length.toNat + 1))).registers := by
  obtain ⟨next, path, result, memory, authority, registers⟩ := counter_increment_path program pc s
    counter one length capacity slot counterValue separate inside first second
  exact ⟨next, path.continues program 2 pc (pc + 2) s next, result, memory, authority, registers⟩

theorem run_counter_increment (program : Array Instruction) (fuel pc : Nat) (s : StateOf Cell)
    (counter one : Nat) (length capacity : UInt64) (slot : NumberSlot s one)
    (counterValue : number s counter = .ok length) (separate : one ≠ counter)
    (inside : length.toNat < capacity.toNat)
    (first : program[pc]? = some (.constant one 1))
    (second : program[pc + 1]? = some (.calculate counter counter one .add)) :
    ∃ next, run program (fuel + 2) pc s = run program fuel (pc + 2) next ∧
      number next counter = .ok (UInt64.ofNat (length.toNat + 1)) ∧
      next.memory = s.memory ∧ next.authority = s.authority := by
  obtain ⟨next, execution, counterValue, memory, authority, _registers⟩ := counter_increment_continues program pc s
    counter one length capacity slot counterValue separate inside first second
  exact ⟨next, execution fuel, counterValue, memory, authority⟩

end Witchy.Core

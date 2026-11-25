import Witchy.StorageStateInvariant
import Witchy.BorrowRootEffects
import Witchy.CoreCounter
import Witchy.CoreLoopControl

namespace Witchy.Core

/-- Checked scalar updates change neither heap ownership nor registered loan
    roots. An owner/reference/capability destination is never overwritten. -/
theorem putCopy_storage_invariant (s next : StateOf Value) (dst : Nat) (value : Value)
    (known : StorageStateInvariant s) (accepted : putCopy s dst value = .ok next) :
    StorageStateInvariant next := by
  have roots := putCopy_loan_roots s next dst value known.roots accepted
  have counts := fun resource => putCopy_register_count s next dst value resource accepted
  unfold putCopy at accepted
  split at accepted <;> try cases accepted
  all_goals
    constructor
    · intro resource
      change registerCount _ resource + _ = _
      rw [counts resource]
      exact known.inventory resource
    · exact known.structural
    · constructor
      · apply Storage.cells_caps_set _ _ _ _ known.capabilities.1
        intro other same
        cases same
        trivial
      · exact known.capabilities.2
    · exact roots

theorem with_number_storage_invariant (s : StateOf Value) (index : Nat) (value : UInt64)
    (known : StorageStateInvariant s) (slot : NumberSlot s index) :
    StorageStateInvariant (withNumber s index value) :=
  putCopy_storage_invariant s _ index (.number value) known (put_number_effect s index value slot)

theorem with_bool_storage_invariant (s : StateOf Value) (index : Nat) (value : Bool)
    (known : StorageStateInvariant s) (slot : BoolSlot s index) :
    StorageStateInvariant (withBool s index value) :=
  putCopy_storage_invariant s _ index (.boolean value) known (put_bool_effect s index value slot)

/-- The emitted constant-one / increment pair preserves the resource-valued
    storage invariant and derives its checked nonoverflowing word result. -/
theorem counter_increment_storage (program : Array Instruction) (pc : Nat) (s : StateOf Value)
    (counter one : Nat) (length limit : UInt64) (slot : NumberSlot s one)
    (counterValue : number s counter = .ok length) (separate : one ≠ counter)
    (inside : length.toNat < limit.toNat) (known : StorageStateInvariant s)
    (first : program[pc]? = some (.constant one 1))
    (second : program[pc + 1]? = some (.calculate counter counter one .add)) :
    ∃ next, Continues program 2 pc s (pc + 2) next ∧ StorageStateInvariant next ∧
      number next counter = .ok (UInt64.ofNat (length.toNat + 1)) ∧
      next.memory = s.memory ∧ next.authority = s.authority ∧
      next.registers = (withNumber (withNumber s one 1) counter (UInt64.ofNat (length.toNat + 1))).registers := by
  obtain ⟨next, execution, result, memory, authority, registers⟩ :=
    counter_increment_continues program pc s counter one length limit slot counterValue separate inside first second
  have retained := (with_number_other_number s one counter 1 separate).trans counterValue
  have preserved := with_number_storage_invariant (withNumber s one 1) counter (UInt64.ofNat (length.toNat + 1))
    (with_number_storage_invariant s one 1 known slot)
    (number_slot_from_number _ counter length retained)
  have same : next = withNumber (withNumber s one 1) counter (UInt64.ofNat (length.toNat + 1)) :=
    state_fields_equal next _ registers memory authority
  exact ⟨next, execution, same.symm ▸ preserved, result, memory, authority, registers⟩

end Witchy.Core

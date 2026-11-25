import Witchy.CoreLoopControl
import Witchy.AllocationStorage

namespace Witchy.Core

variable {Cell : Type} [StorageCell Cell]

/-- Successful recoverable allocation fills the owner and success tag. The error
    register is checked for scalar writability but not changed on this branch. -/
theorem try_allocate_success_effect_of (s : StateOf Cell) (tag owner error count : Nat)
    (size : UInt64) (handle : Memory.Handle)
    (separate : tag ≠ owner ∧ tag ≠ error ∧ owner ≠ error)
    (ownerEmpty : vacant s owner = .ok ()) (tagSlot : BoolSlot s tag)
    (errorSlot : NumberSlot s error) (countValue : number s count = .ok size)
    (allocated : (Memory.allocate s.memory size.toNat).2 = .ok handle) :
    step s (.tryAllocate tag owner error count) = .ok
      (withBool { s with
        memory := (Memory.allocate s.memory size.toNat).1
        registers := s.registers.set owner (some (.owner handle)) } tag true) := by
  have previewTag := put_bool_effect s tag false tagSlot
  have previewError := put_number_effect s error 0 errorSlot
  have stillEmpty : vacant { s with memory := (Memory.allocate s.memory size.toNat).1 } owner = .ok () := ownerEmpty
  let owned : StateOf Cell := { s with
    memory := (Memory.allocate s.memory size.toNat).1
    registers := s.registers.set owner (some (.owner handle)) }
  have ownedTag : BoolSlot owned tag := by
    simpa only [BoolSlot, owned, List.getElem?_set_ne (Ne.symm separate.1)] using tagSlot
  have result := put_bool_effect owned tag true ownedTag
  simp only [step, separate.1, separate.2.1, separate.2.2, or_self, ↓reduceIte, ownerEmpty,
    previewTag, previewError, countValue, allocated, Bind.bind, Pure.pure, put, stillEmpty]
  exact result

/-- Both recoverable refusal reasons have explicit scalar error codes. The old
    memory and all owner/reference registers remain untouched. -/
theorem try_allocate_refusal_effect_of (s : StateOf Cell) (tag owner error count : Nat)
    (size : UInt64) (reason : Memory.Error)
    (separate : tag ≠ owner ∧ tag ≠ error ∧ owner ≠ error)
    (ownerEmpty : vacant s owner = .ok ()) (tagSlot : BoolSlot s tag)
    (errorSlot : NumberSlot s error) (countValue : number s count = .ok size)
    (refused : (Memory.allocate s.memory size.toNat).2 = .error reason) :
    step s (.tryAllocate tag owner error count) = .ok
      (withBool (withNumber s error (if reason = .allocation then 0 else 1)) tag false) := by
  have previewTag := put_bool_effect s tag false tagSlot
  have previewError := put_number_effect s error 0 errorSlot
  have result (code : UInt64) :
      putCopy (withNumber s error code) tag (.boolean false) =
        .ok (withBool (withNumber s error code) tag false) := by
    apply put_bool_effect
    simpa only [BoolSlot, withNumber, List.getElem?_set_ne (Ne.symm separate.2.1)] using tagSlot
  rcases Memory.allocation_error_kind_of s.memory size.toNat reason refused with quota | metadata
  · subst reason
    simp only [step, separate.1, separate.2.1, separate.2.2, or_self, ↓reduceIte,
      ownerEmpty, previewTag, previewError, countValue, refused, Bind.bind]
    exact result 0
  · subst reason
    simp only [step, separate.1, separate.2.1, separate.2.2, or_self, ↓reduceIte,
      ownerEmpty, previewTag, previewError, countValue, refused, Bind.bind]
    rw [put_number_effect s error 1 errorSlot]
    exact result 1

/-- Initialized count and writable result slots make recoverable allocation
    executable for either allocator outcome. No success/budget premise. -/
theorem try_allocate_step_of (s : StateOf Cell) (tag owner error count : Nat) (size : UInt64)
    (separate : tag ≠ owner ∧ tag ≠ error ∧ owner ≠ error)
    (ownerEmpty : vacant s owner = .ok ()) (tagSlot : BoolSlot s tag)
    (errorSlot : NumberSlot s error) (countValue : number s count = .ok size) :
    ∃ next, step s (.tryAllocate tag owner error count) = .ok next := by
  cases outcome : (Memory.allocate s.memory size.toNat).2 with
  | ok root =>
    exact ⟨_, try_allocate_success_effect_of s tag owner error count size root separate ownerEmpty tagSlot errorSlot countValue outcome⟩
  | error reason =>
    exact ⟨_, try_allocate_refusal_effect_of s tag owner error count size reason separate ownerEmpty tagSlot errorSlot countValue outcome⟩

theorem try_allocate_success_effect (s : State) (tag owner error count : Nat)
    (size : UInt64) (handle : Memory.Handle)
    (separate : tag ≠ owner ∧ tag ≠ error ∧ owner ≠ error)
    (ownerEmpty : vacant s owner = .ok ()) (tagSlot : BoolSlot s tag)
    (errorSlot : NumberSlot s error) (countValue : number s count = .ok size)
    (allocated : (Memory.allocate s.memory size.toNat).2 = .ok handle) :
    step s (.tryAllocate tag owner error count) = .ok
      (withBool { s with
        memory := (Memory.allocate s.memory size.toNat).1
        registers := s.registers.set owner (some (.owner handle)) } tag true) := by
  exact try_allocate_success_effect_of s tag owner error count size handle separate ownerEmpty tagSlot errorSlot countValue allocated

theorem try_allocate_refusal_effect (s : State) (tag owner error count : Nat)
    (size : UInt64) (reason : Memory.Error)
    (separate : tag ≠ owner ∧ tag ≠ error ∧ owner ≠ error)
    (ownerEmpty : vacant s owner = .ok ()) (tagSlot : BoolSlot s tag)
    (errorSlot : NumberSlot s error) (countValue : number s count = .ok size)
    (refused : (Memory.allocate s.memory size.toNat).2 = .error reason) :
    step s (.tryAllocate tag owner error count) = .ok
      (withBool (withNumber s error (if reason = .allocation then 0 else 1)) tag false) := by
  exact try_allocate_refusal_effect_of s tag owner error count size reason separate ownerEmpty tagSlot errorSlot countValue refused

end Witchy.Core

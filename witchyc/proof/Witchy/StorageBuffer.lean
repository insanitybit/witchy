import Witchy.StorageBlock

namespace Witchy.Storage

/-- A buffer stores its physical initialization state, not just a mathematical
    contents list. Access/allocation ownership is supplied by the enclosing API. -/
structure Buffer (Value : Type) where
  block : Block Value
  length : Nat
  deriving Repr, DecidableEq

def Buffer.Represents (buffer : Buffer Value) (values : List Value) (spare : Nat) : Prop :=
  buffer.length = values.length ∧
    buffer.block.slots = values.map some ++ List.replicate spare none

/-- A failed local transfer preserves the buffer. Under Represents, only the
    empty case fails. The returned payload is transferred, with no Copy bound. -/
def Buffer.pop (buffer : Buffer Value) : Buffer Value × Option Value :=
  if buffer.length = 0 then (buffer, none)
  else match buffer.block.take (buffer.length - 1) with
    | (_, none) => (buffer, none)
    | (block, some value) => (⟨block, buffer.length - 1⟩, some value)

/-- Even an invalid initialization shape cannot make a refused pop consume
    the buffer or silently decrement its length. -/
theorem Buffer.pop_refused (buffer next : Buffer Value)
    (refused : buffer.pop = (next, none)) : next = buffer := by
  unfold Buffer.pop at refused
  split at refused
  · cases refused; rfl
  · split at refused
    · cases refused; rfl
    · cases refused

theorem Buffer.capacity (buffer : Buffer Value) (values : List Value) (spare : Nat)
    (represented : buffer.Represents values spare) :
    buffer.block.slots.length = values.length + spare := by
  rw [represented.2]
  simp

theorem Buffer.pop_empty (buffer : Buffer Value) (spare : Nat)
    (represented : buffer.Represents [] spare) : buffer.pop = (buffer, none) := by
  simp [Buffer.pop, represented.1]

/-- General successful pop: return the last owned value, clear exactly its
    storage cell, retain the preceding values and increase spare capacity. -/
theorem Buffer.take_last_initialized (buffer : Buffer Value) (front : List Value) (value : Value) (spare : Nat)
    (represented : buffer.Represents (front ++ [value]) spare) :
    ∃ next, buffer.pop = (next, some value) ∧ next.Represents front (spare + 1) ∧
      next.block.slots.length = buffer.block.slots.length := by
  have length : buffer.length = front.length + 1 := by simpa using represented.1
  have shape : buffer.block = ⟨front.map some ++ some value :: List.replicate spare none⟩ := by
    cases buffer with
    | mk block used =>
      cases block with
      | mk slots =>
        have same := represented.2
        simp only [List.map_append, List.map_cons, List.map_nil] at same
        cases same
        simp [List.append_assoc]
  have transfer := Block.take_split (front.map some) (List.replicate spare none) value
  let next : Buffer Value := ⟨⟨front.map some ++ none :: List.replicate spare none⟩, front.length⟩
  refine ⟨next, ?_, ?_, ?_⟩
  · simp only [Buffer.pop, length, Nat.add_sub_cancel, Nat.add_eq_zero_iff, Nat.one_ne_zero,
      and_false, ↓reduceIte, shape, List.length_map] at *
    rw [transfer]
  · constructor
    · rfl
    · simp [next, List.replicate_succ]
  · rw [Buffer.capacity buffer (front ++ [value]) spare represented]
    simp [next]
    omega

end Witchy.Storage

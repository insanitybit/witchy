import Witchy.StorageBuffer

namespace Witchy.Storage

/-- Insertion without growth. Failure returns ownership of the input value
    and the unchanged buffer, including malformed or already initialized slots.
    This local Nat metadata model does not yet establish word-size bounds. -/
def Buffer.push (buffer : Buffer Value) (value : Value) : Buffer Value × Option Value :=
  match buffer.block.slots[buffer.length]? with
  | some none => (⟨⟨buffer.block.slots.set buffer.length (some value)⟩, buffer.length + 1⟩, none)
  | _ => (buffer, some value)

theorem Buffer.push_refused (buffer next : Buffer Value) (value returned : Value)
    (refused : buffer.push value = (next, some returned)) : next = buffer ∧ returned = value := by
  unfold Buffer.push at refused
  split at refused
  · cases refused
  · cases refused; exact ⟨rfl, rfl⟩

/-- Full buffers preserve their contents and return the uninserted value. -/
theorem Buffer.push_full (buffer : Buffer Value) (values : List Value) (value : Value)
    (represented : buffer.Represents values 0) : buffer.push value = (buffer, some value) := by
  have shape : buffer.block.slots = values.map some := by simpa using represented.2
  simp [Buffer.push, represented.1, shape]

/-- Successful insertion consumes one spare cell and preserves all earlier
    payloads and physical capacity, for arbitrary Value without Copy. -/
theorem Buffer.push_spare (buffer : Buffer Value) (values : List Value) (value : Value) (spare : Nat)
    (represented : buffer.Represents values (spare + 1)) :
    ∃ next, buffer.push value = (next, none) ∧ next.Represents (values ++ [value]) spare ∧
      next.block.slots.length = buffer.block.slots.length := by
  have shape : buffer.block.slots = values.map some ++ none :: List.replicate spare none := by
    simpa [List.replicate_succ] using represented.2
  let next : Buffer Value := ⟨⟨values.map some ++ some value :: List.replicate spare none⟩, values.length + 1⟩
  refine ⟨next, ?_, ?_, ?_⟩
  · simp [Buffer.push, represented.1, shape, next]
  · constructor
    · simp [next]
    · simp [next, List.map_append, List.append_assoc]
  · rw [Buffer.capacity buffer values (spare + 1) represented]
    simp [next]

/-- Insertion followed by removal returns the input value and exactly restores
    the local buffer, not merely its length or initialized-cell count. -/
theorem Buffer.push_pop (buffer : Buffer Value) (values : List Value) (value : Value) (spare : Nat)
    (represented : buffer.Represents values (spare + 1)) :
    ∃ inserted, buffer.push value = (inserted, none) ∧ inserted.pop = (buffer, some value) := by
  obtain ⟨inserted, pushed, insertion, _capacity⟩ := Buffer.push_spare buffer values value spare represented
  obtain ⟨restored, popped, restoration, _capacity⟩ := Buffer.take_last_initialized inserted values value spare insertion
  have lengths : restored.length = buffer.length := restoration.1.trans represented.1.symm
  have slots : restored.block.slots = buffer.block.slots := restoration.2.trans represented.2.symm
  have same : restored = buffer := by
    cases restored with
    | mk restoredBlock restoredLength =>
      cases buffer with
      | mk originalBlock originalLength =>
        cases restoredBlock
        cases originalBlock
        simp_all
  exact ⟨inserted, pushed, by simpa only [same] using popped⟩

end Witchy.Storage

import Witchy.LogicTakeRule
import Witchy.LogicEndRule

namespace Witchy.Logic.SourceMemory

/-- A read observes the initialized cell without changing any heap resource.
    Scalar decoding/copy eligibility is the trusted source type boundary. -/
theorem read_cell (memory : Memory.StateOf Core.Value) (access : Memory.Access)
    (handle index : Nat) (block : Storage.Block Core.Value) (value : Core.Value)
    (permission : Memory.resolve memory access false = .ok handle)
    (stored : Memory.block? memory handle = some block)
    (initialized : block.slots[index]? = some (some value)) :
    Memory.read memory access index = .ok value := by
  simp [Memory.read, permission, stored, initialized]

/-- Numeric consuming access has the same genuine cell-removal effect before
    scalar decoding. No other allocation, loan or authority is changed. -/
theorem take_cell (memory : Memory.StateOf Core.Value) (access : Memory.Access)
    (handle index : Nat) (block : Storage.Block Core.Value) (value : Core.Value)
    (permission : Memory.resolve memory access true = .ok handle)
    (stored : Memory.block? memory handle = some block)
    (initialized : block.slots[index]? = some (some value)) :
    Memory.take memory access index =
      ({ memory with blocks := memory.blocks.set handle (some ⟨block.slots.set index none⟩) }, .ok value) := by
  simp [Memory.take, permission, stored, initialized]

/-- Contents of an existing owned resource, not a replacement heap model. -/
def Cell (resource : Resource) (handle index : Nat) (value : Option Core.Value) : Prop :=
  ∃ block, resource (.block handle) = some block ∧ block.slots[index]? = some value

/-- Extraction exposes entry-cell identity, the emptied cell, and every other
    resource unchanged. The underlying rule also preserves the identical frame. -/
theorem take_contents_rule (dst src index handle : Nat) (offset : UInt64)
    (block : Storage.Block Core.Value) (value : Core.Value) :
    AtomicRule (.takeMove dst src index)
      (fun resource => TakeInput resource dst src index handle offset block value)
      (fun before outcome after => TakeOutput before dst handle offset block value outcome after ∧
        Cell before handle offset.toNat (some value) ∧ Cell after handle offset.toNat none) := by
  apply (take_rule dst src index handle offset block value).map
  · intro resource input
    exact input
  · intro before outcome after input output
    refine ⟨output, ⟨block, input.storage, input.initialized⟩, ?_⟩
    refine ⟨⟨block.slots.set offset.toNat none⟩, output.2.2.1, ?_⟩
    exact List.getElem?_set_self (List.getElem?_eq_some_iff.mp input.initialized).1

/-- Definedness of an extracted cell supplies its genuine value and bounds. -/
theorem initialized_slot_exists (block : Storage.Block Core.Value) (index : Nat)
    (ready : (block.slots[index]?.bind id).isSome = true) :
    ∃ value, block.slots[index]? = some (some value) := by
  cases observed : block.slots[index]? with
  | none => simp [observed] at ready
  | some value =>
    cases value with
    | none => simp [observed] at ready
    | some value => exact ⟨value, rfl⟩

/-- Restoring the genuine observed initialized payload changes no cell. -/
@[simp] theorem set_initialized_observation (slots : List (Option Core.Value)) (index : Nat)
    (ready : (slots[index]?.bind id).isSome = true) :
    slots.set index (slots[index]?.bind id) = slots := by
  obtain ⟨value, found⟩ := initialized_slot_exists ⟨slots⟩ index ready
  replace found : slots[index]? = some (some value) := found
  obtain ⟨bound, element⟩ := List.getElem?_eq_some_iff.mp found
  have selected : slots[index]?.bind id = some value := by simp [found]
  rw [selected, ← element]
  exact List.set_getElem_self bound

/-- A universally quantified contents VC can be discharged independently of
    register placement, heap size, and unrelated cells. -/
theorem emptied_at (block : Storage.Block Core.Value) (index : Nat) (value : Core.Value)
    (initialized : block.slots[index]? = some (some value)) :
    (block.slots.set index none)[index]? = some none :=
  List.getElem?_set_self (List.getElem?_eq_some_iff.mp initialized).1

end Witchy.Logic.SourceMemory

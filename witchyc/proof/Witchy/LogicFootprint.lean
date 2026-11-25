import Witchy.LogicObservation
import Witchy.MemoryContents

namespace Witchy.Logic

/-- Primitive effects preserve every observation outside their write footprint.
    This is a store lemma, not yet the program-logic frame theorem. -/
def UnchangedOutside (written : List Location) (before after : Core.StateOf Core.Value) : Prop :=
  ∀ location, location ∉ written → observe after location = observe before location

theorem patch_register_block (store : Core.StateOf Core.Value) (register handle : Nat)
    (value : Option Core.Value) (block : Storage.Block Core.Value)
    (live : Memory.block? store.memory handle ≠ none) :
    UnchangedOutside [.register register, .block handle] store
      { store with
        registers := store.registers.set register value
        memory := { store.memory with blocks := store.memory.blocks.set handle (some block) } } := by
  intro location outside
  cases location with
  | register index =>
    have different : register ≠ index := by
      intro same
      subst index
      simp at outside
    exact List.getElem?_set_ne different
  | block other =>
    have different : handle ≠ other := by
      intro same
      subst other
      simp at outside
    exact congrArg (fun cell : Option (Option (Storage.Block Core.Value)) => cell.bind id)
      (List.getElem?_set_ne different)
  | loan identity => rfl
  | permissions target =>
    by_cases same : handle = target
    · subst target
      cases found : Memory.block? store.memory handle with
      | none => exact False.elim (live found)
      | some old =>
        change (Memory.block? { store.memory with blocks := store.memory.blocks.set handle (some block) } handle).map
          (fun _ => Memory.active store.memory handle) =
            (Memory.block? store.memory handle).map (fun _ => Memory.active store.memory handle)
        rw [Memory.block_after_set store.memory handle old block found, found] <;> rfl
    · exact congrArg (fun entry => entry.map (fun _ => Memory.active store.memory target))
        (congrArg (fun cell : Option (Option (Storage.Block Core.Value)) => cell.bind id)
          (List.getElem?_set_ne same))
  | allocator => simp [observe]
  | loanSupply => rfl
  | authority => rfl

end Witchy.Logic

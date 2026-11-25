import Witchy.LogicAccess
import Witchy.LogicTake

namespace Witchy.Logic

structure TakeInput (resource : Resource) (dst src index handle : Nat) (offset : UInt64)
    (block : Storage.Block Core.Value) (value : Core.Value) : Prop where
  destination : resource (.register dst) = some none
  indexValue : resource (.register index) = some (some (.number offset))
  access : WritableAccess resource src handle
  storage : resource (.block handle) = some block
  initialized : block.slots[offset.toNat]? = some (some value)
  movable : Movable resource value

def TakeOutput (before : Resource) (dst handle : Nat) (offset : UInt64)
    (block : Storage.Block Core.Value) (value : Core.Value) (result : Core.Result Unit) (after : Resource) : Prop :=
  result = .ok () ∧ after (.register dst) = some (some value) ∧
    after (.block handle) = some ⟨block.slots.set offset.toNat none⟩ ∧
    Resource.SameOutside [.register dst, .block handle] before after

/-- The public extraction rule has resource-only pre/postconditions. It
    neither names a whole store nor receives its execution as a premise. -/
theorem take_rule (dst src index handle : Nat) (offset : UInt64)
    (block : Storage.Block Core.Value) (value : Core.Value) :
    AtomicRule (.takeMove dst src index)
      (fun resource => TakeInput resource dst src index handle offset block value)
      (fun before => TakeOutput before dst handle offset block value) := by
  apply AtomicRule.of_effect
  intro resource store input _valid known
  have empty := known (.register dst) none input.destination
  have position := known (.register index) (some (.number offset)) input.indexValue
  have storage := known (.block handle) block input.storage
  obtain ⟨permission, accessed, resolved⟩ := WritableAccess.realize resource store src handle known input.access
  have movable := Movable.realize resource store value known input.movable
  change store.registers[index]? = some (some (.number offset)) at position
  have afterMovable : Core.TransferUnborrowed
      { store.memory with blocks := store.memory.blocks.set handle (some ⟨block.slots.set offset.toNat none⟩) } value := by
    cases value <;> exact movable
  obtain ⟨next, performed, effect, output, contents⟩ := take_locality store dst src index handle offset
    permission block value empty accessed (by simp [Core.number, Core.get, position, Bind.bind, Pure.pure])
      resolved storage input.initialized afterMovable
  rw [performed]
  refine ⟨[.register dst, .block handle], ?_, effect, ?_⟩
  · intro location member
    simp only [List.mem_cons, List.not_mem_nil, or_false] at member
    rcases member with rfl | rfl
    · exact .inl (Resource.occupied input.destination)
    · exact .inl (Resource.occupied input.storage)
  · refine ⟨rfl, ?_, ?_, Resource.patch_same_outside _ _ _⟩
    · exact (Resource.patch_at _ _ _ _ (by simp)).trans output
    · exact (Resource.patch_at _ _ _ _ (by simp)).trans contents

end Witchy.Logic

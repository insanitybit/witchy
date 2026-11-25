import Witchy.LogicAccess
import Witchy.LogicInit

namespace Witchy.Logic

structure InitInput (resource : Resource) (dst index source handle : Nat) (offset : UInt64)
    (block : Storage.Block Core.Value) (value : Core.Value) : Prop where
  sourceValue : resource (.register source) = some (some value)
  indexValue : resource (.register index) = some (some (.number offset))
  access : WritableAccess resource dst handle
  storage : resource (.block handle) = some block
  vacant : block.slots[offset.toNat]? = some none
  movable : Movable resource value
  notSelf : Core.TransferNotSelf handle value

def InitOutput (before : Resource) (source handle : Nat) (offset : UInt64)
    (block : Storage.Block Core.Value) (value : Core.Value) (result : Core.Result Unit) (after : Resource) : Prop :=
  result = .ok () ∧ after (.register source) = some none ∧
    after (.block handle) = some ⟨block.slots.set offset.toNat (some value)⟩ ∧
    Resource.SameOutside [.register source, .block handle] before after

theorem init_rule (dst index source handle : Nat) (offset : UInt64)
    (block : Storage.Block Core.Value) (value : Core.Value) :
    AtomicRule (.storeMove dst index source)
      (fun resource => InitInput resource dst index source handle offset block value)
      (fun before => InitOutput before source handle offset block value) := by
  apply AtomicRule.of_effect
  intro resource store input _valid known
  have present := known (.register source) (some value) input.sourceValue
  have position := known (.register index) (some (.number offset)) input.indexValue
  have storage := known (.block handle) block input.storage
  obtain ⟨permission, accessed, resolved⟩ := WritableAccess.realize resource store dst handle known input.access
  have movable := Movable.realize resource store value known input.movable
  change store.registers[index]? = some (some (.number offset)) at position
  obtain ⟨next, performed, effect, output, contents⟩ := init_locality store dst index source handle offset
    permission block value present accessed (by simp [Core.number, Core.get, position, Bind.bind, Pure.pure])
      resolved storage input.vacant movable input.notSelf
  rw [performed]
  refine ⟨[.register source, .block handle], ?_, effect, ?_⟩
  · intro location member
    simp only [List.mem_cons, List.not_mem_nil, or_false] at member
    rcases member with rfl | rfl
    · exact .inl (Resource.occupied input.sourceValue)
    · exact .inl (Resource.occupied input.storage)
  · refine ⟨rfl, ?_, ?_, Resource.patch_same_outside _ _ _⟩
    · exact (Resource.patch_at _ _ _ _ (by simp)).trans output
    · exact (Resource.patch_at _ _ _ _ (by simp)).trans contents

end Witchy.Logic

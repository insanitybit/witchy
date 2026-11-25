import Witchy.LogicScalar
import Witchy.LogicTakeRule
import Witchy.LogicInitRule

namespace Witchy.Logic

theorem Resource.SameOutside.one (before after : Resource) (location : Location)
    (value : Option location.Data) (written : after location = value)
    (unchanged : Resource.SameOutside [location] before after) :
    after = before.set location value := by
  funext other
  by_cases same : other = location
  · subst other; simpa using written
  · simpa only [Resource.set_other before location other value same] using unchanged other (by simp [same])

theorem Resource.SameOutside.two (before after : Resource) (first second : Location)
    (firstValue : Option first.Data) (secondValue : Option second.Data)
    (firstWritten : after first = firstValue) (secondWritten : after second = secondValue)
    (_different : first ≠ second) (unchanged : Resource.SameOutside [first, second] before after) :
    after = (before.set first firstValue).set second secondValue := by
  funext other
  by_cases sameSecond : other = second
  · subst other; simpa using secondWritten
  · rw [Resource.set_other _ second other secondValue sameSecond]
    by_cases sameFirst : other = first
    · subst other; simpa using firstWritten
    · rw [Resource.set_other _ first other firstValue sameFirst]
      exact unchanged other (by simp [sameFirst, sameSecond])

theorem ScalarOutput.updated (dst : Nat) (value : Core.Value) (before after : Resource)
    (known : ScalarOutput dst value before (.ok ()) after) :
    after = before.set (.register dst) (some (some value)) :=
  known.2.2.one before after (.register dst) _ known.2.1

theorem TakeOutput.updated (before after : Resource) (dst handle : Nat) (offset : UInt64)
    (block : Storage.Block Core.Value) (value : Core.Value)
    (known : TakeOutput before dst handle offset block value (.ok ()) after) :
    after = (before.set (.register dst) (some (some value))).set (.block handle)
      (some ⟨block.slots.set offset.toNat none⟩) :=
  known.2.2.2.two before after (.register dst) (.block handle) _ _ known.2.1 known.2.2.1 (by simp)

theorem InitOutput.updated (before after : Resource) (src handle : Nat) (offset : UInt64)
    (block : Storage.Block Core.Value) (value : Core.Value)
    (known : InitOutput before src handle offset block value (.ok ()) after) :
    after = (before.set (.register src) (some none)).set (.block handle)
      (some ⟨block.slots.set offset.toNat (some value)⟩) :=
  known.2.2.2.two before after (.register src) (.block handle) _ _ known.2.1 known.2.2.1 (by simp)

end Witchy.Logic

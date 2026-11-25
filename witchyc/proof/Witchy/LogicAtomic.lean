import Witchy.LogicPatch
import Witchy.LogicInterpretation

namespace Witchy.Logic

/-- An atomic rule accounts for both actual instruction outcomes. On fault the
    store/resources are retained. A fault is not a successful function return;
    total region proofs must eliminate it or take a real recoverable branch. -/
def AtomicRule (instruction : Core.Instruction) (pre : Resource → Prop)
    (post : Resource → Core.Result Unit → Resource → Prop) : Prop :=
  ∀ ownedPart frame store, pre ownedPart → Resource.Disjoint ownedPart frame →
    StoreRealizes store (Resource.join ownedPart frame) →
    match Core.step store instruction with
    | .error reason => post ownedPart (.error reason) ownedPart
    | .ok next => ∃ output, post ownedPart (.ok ()) output ∧ Resource.Disjoint output frame ∧
        StoreRealizes next (Resource.join output frame)

/-- All primitive wrappers use this same proof of interpretation preservation.
    Their own obligations contain local data and an actual checked step effect. -/
theorem AtomicRule.of_effect (instruction : Core.Instruction) (pre : Resource → Prop)
    (post : Resource → Core.Result Unit → Resource → Prop)
    (derive : ∀ ownedPart store, pre ownedPart → ValidStore store → Agrees store ownedPart →
      match Core.step store instruction with
      | .error reason => post ownedPart (.error reason) ownedPart
      | .ok next => ∃ written, AllowsWrites store ownedPart written ∧ UnchangedOutside written store next ∧
          post ownedPart (.ok ()) (ownedPart.patch written (observe next))) :
    AtomicRule instruction pre post := by
  intro ownedPart frame store input apart known
  have agreement := StoreRealizes.agrees store _ known
  have localKnown := ((agrees_join store ownedPart frame apart).mp agreement).1
  have derived := derive ownedPart store input known.1 localKnown
  cases performed : Core.step store instruction with
  | error reason => simpa only [performed] using derived
  | ok next =>
    rw [performed] at derived
    obtain ⟨written, allowed, effect, output⟩ := derived
    have nextApart := (patch_locality store next ownedPart frame written apart agreement allowed effect).1
    have nextTotal := patch_complete store next ownedPart frame written apart known.2 allowed effect
    exact ⟨_, output, nextApart, ValidStore.step store next instruction known.1 performed, nextTotal⟩

theorem AtomicRule.strengthen {instruction basic post} (rule : AtomicRule instruction basic post)
    (pre : Resource → Prop) (input : ∀ resource, pre resource → basic resource) :
    AtomicRule instruction pre post := by
  intro resource frame store known apart realized
  exact rule resource frame store (input resource known) apart realized

theorem AtomicRule.map {instruction basic post} (rule : AtomicRule instruction basic post)
    (pre : Resource → Prop) (result : Resource → Core.Result Unit → Resource → Prop)
    (input : ∀ resource, pre resource → basic resource)
    (output : ∀ before outcome after, pre before → post before outcome after → result before outcome after) :
    AtomicRule instruction pre result := by
  intro resource frame store known apart realized
  have derived := rule resource frame store (input resource known) apart realized
  cases performed : Core.step store instruction with
  | error reason => rw [performed] at derived; exact output resource (.error reason) resource known derived
  | ok next =>
    rw [performed] at derived
    obtain ⟨after, produced, afterApart, final⟩ := derived
    exact ⟨after, output resource (.ok ()) after known produced, afterApart, final⟩

end Witchy.Logic

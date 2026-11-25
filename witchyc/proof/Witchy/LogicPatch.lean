import Witchy.LogicFootprint

namespace Witchy.Logic

def Resource.patch (resource : Resource) (written : List Location) (snapshot : Resource) : Resource :=
  fun location => if location ∈ written then snapshot location else resource location

theorem Resource.patch_at (resource snapshot : Resource) (written : List Location) (location : Location)
    (member : location ∈ written) : resource.patch written snapshot location = snapshot location := by
  simp only [patch, member, ↓reduceIte]

def Resource.SameOutside (written : List Location) (before after : Resource) : Prop :=
  ∀ location, location ∉ written → after location = before location

theorem Resource.patch_same_outside (resource snapshot : Resource) (written : List Location) :
    SameOutside written resource (resource.patch written snapshot) := by
  intro location outside
  simp only [patch, outside, ↓reduceIte]

theorem Resource.SameOutside.refl (written : List Location) (resource : Resource) : SameOutside written resource resource :=
  fun _ _ => rfl

theorem Resource.SameOutside.trans (written : List Location) (before middle after : Resource)
    (first : SameOutside written before middle) (second : SameOutside written middle after) :
    SameOutside written before after := fun location outside => (second location outside).trans (first location outside)

theorem Resource.SameOutside.widen (small large : List Location) (before after : Resource)
    (known : SameOutside small before after) (subset : ∀ location ∈ small, location ∈ large) :
    SameOutside large before after := fun location outside => known location (fun member => outside (subset location member))

theorem Resource.SameOutside.set (written : List Location) (original before : Resource)
    (known : SameOutside written original before) (location : Location) (value : Option location.Data)
    (member : location ∈ written) : SameOutside written original (before.set location value) := by
  intro other outside
  rw [Resource.set_other _ _ _ _ (by intro same; subst other; exact outside member)]
  exact known other outside

/-- Writes require local ownership, except a genuinely absent fresh resource.
    An absent register value is still an owned register entry. -/
def AllowsWrites (store : Core.StateOf Core.Value) (ownedPart : Resource) (written : List Location) : Prop :=
  ∀ location ∈ written, ownedPart location ≠ none ∨ observe store location = none

/-- Lift a checked store footprint to preservation of an arbitrary disjoint
    resource fragment. This is the primitive lifting lemma, not a region rule. -/
theorem patch_locality (before after : Core.StateOf Core.Value) (ownedPart frame : Resource)
    (written : List Location) (apart : Resource.Disjoint ownedPart frame)
    (known : Agrees before (Resource.join ownedPart frame))
    (allowed : AllowsWrites before ownedPart written) (effect : UnchangedOutside written before after) :
    Resource.Disjoint (ownedPart.patch written (observe after)) frame ∧
      Agrees after (Resource.join (ownedPart.patch written (observe after)) frame) := by
  obtain ⟨localKnown, frameKnown⟩ := (agrees_join before ownedPart frame apart).mp known
  have frameEmpty (location : Location) (member : location ∈ written) : frame location = none := by
    rcases allowed location member with owned | fresh
    · rcases apart location with empty | empty
      · exact False.elim (owned empty)
      · exact empty
    · cases found : frame location with
      | none => rfl
      | some value =>
        have present := frameKnown location value found
        rw [fresh] at present
        cases present
  have nextApart : Resource.Disjoint (ownedPart.patch written (observe after)) frame := by
    intro location
    by_cases member : location ∈ written
    · exact .inr (frameEmpty location member)
    · simpa only [Resource.patch, member, ↓reduceIte] using apart location
  refine ⟨nextApart, (agrees_join after _ frame nextApart).mpr ⟨?_, ?_⟩⟩
  · intro location value found
    by_cases member : location ∈ written
    · simpa only [Resource.patch, member, ↓reduceIte] using found
    · have previous : ownedPart location = some value := by
        simpa only [Resource.patch, member, ↓reduceIte] using found
      exact (effect location member).trans (localKnown location value previous)
  · intro location value found
    have outside : location ∉ written := by
      intro member
      have empty := frameEmpty location member
      rw [found] at empty
      cases empty
    exact (effect location outside).trans (frameKnown location value found)

/-- Completeness is preserved as well as agreement: a rule cannot silently
    drop live resources from the logical ownership decomposition. -/
theorem patch_complete (before after : Core.StateOf Core.Value) (ownedPart frame : Resource)
    (written : List Location) (apart : Resource.Disjoint ownedPart frame)
    (total : Resource.join ownedPart frame = observe before)
    (allowed : AllowsWrites before ownedPart written) (effect : UnchangedOutside written before after) :
    Resource.join (ownedPart.patch written (observe after)) frame = observe after := by
  funext location
  by_cases member : location ∈ written
  · have empty : frame location = none := by
      rcases allowed location member with owned | fresh
      · rcases apart location with missing | missing
        · exact False.elim (owned missing)
        · exact missing
      · have combined := congrFun total location
        rw [fresh] at combined
        cases first : ownedPart location with
        | none => simpa only [Resource.join, first] using combined
        | some value => simp only [Resource.join, first] at combined; cases combined
    simp only [Resource.patch, member, ↓reduceIte, Resource.join, empty]
    cases observe after location <;> rfl
  · have combined := congrFun total location
    change (match ownedPart.patch written (observe after) location with
      | none => frame location | some value => some value) = observe after location
    simp only [Resource.patch, member, ↓reduceIte]
    exact combined.trans (effect location member).symm

end Witchy.Logic

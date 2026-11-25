import Witchy.LogicLocation

namespace Witchy.Logic

/-- A partial resource view of the existing store, with typed entries. -/
abbrev Resource := (location : Location) → Option location.Data

namespace Resource

def empty : Resource := fun _ => none

def join (left right : Resource) : Resource := fun location =>
  match left location with
  | none => right location
  | some value => some value

def Disjoint (left right : Resource) : Prop :=
  ∀ location, left location = none ∨ right location = none

def single (location : Location) (value : location.Data) : Resource := fun other =>
  if same : other = location then some (same.symm ▸ value) else none

def set (resource : Resource) (location : Location) (value : Option location.Data) : Resource := fun other =>
  if same : other = location then same.symm ▸ value else resource other

theorem occupied {resource : Resource} {location : Location} {value : location.Data}
    (entry : resource location = some value) : resource location ≠ none := by
  intro empty
  rw [entry] at empty
  cases empty

theorem set_apply (resource : Resource) (location : Location) (value : Option location.Data) (other : Location) :
    resource.set location value other = if same : other = location then same.symm ▸ value else resource other := rfl

@[simp] theorem empty_at (location : Location) : empty location = none := rfl

@[simp] theorem single_at (location : Location) (value : location.Data) :
    single location value location = some value := by simp [single]

@[simp] theorem set_at (resource : Resource) (location : Location) (value : Option location.Data) :
    set resource location value location = value := by simp [set]

theorem set_other (resource : Resource) (location other : Location) (value : Option location.Data)
    (different : other ≠ location) : set resource location value other = resource other := by
  simp [set, different]

@[simp] theorem empty_join (resource : Resource) : join empty resource = resource := rfl

@[simp] theorem join_empty (resource : Resource) : join resource empty = resource := by
  funext location
  cases found : resource location <;> simp [join, empty, found]

theorem join_assoc (first second third : Resource) :
    join (join first second) third = join first (join second third) := by
  funext location
  cases found : first location <;> simp [join, found]

theorem Disjoint.symm {left right : Resource} (known : Disjoint left right) : Disjoint right left := by
  intro location
  exact (known location).symm

theorem empty_disjoint (resource : Resource) : Disjoint empty resource := by
  intro location
  exact .inl rfl

theorem join_comm (left right : Resource) (apart : Disjoint left right) : join left right = join right left := by
  funext location
  rcases apart location with empty | empty
  · cases found : right location <;> simp [join, empty, found]
  · cases found : left location <;> simp [join, empty, found]

theorem disjoint_join_right (first second third : Resource) :
    Disjoint first (join second third) ↔ Disjoint first second ∧ Disjoint first third := by
  constructor
  · intro known
    constructor
    · intro location
      rcases known location with empty | empty
      · exact .inl empty
      · right
        cases found : second location with
        | none => rfl
        | some value => simp [join, found] at empty
    · intro location
      rcases known location with empty | empty
      · exact .inl empty
      · right
        cases found : second location with
        | none => simpa [join, found] using empty
        | some value => simp [join, found] at empty
  · rintro ⟨secondApart, thirdApart⟩ location
    rcases secondApart location with empty | secondEmpty
    · exact .inl empty
    · rcases thirdApart location with empty | thirdEmpty
      · exact .inl empty
      · exact .inr (by simp [join, secondEmpty, thirdEmpty])

theorem disjoint_join_left (first second third : Resource) :
    Disjoint (join first second) third ↔ Disjoint first third ∧ Disjoint second third := by
  constructor
  · intro known
    obtain ⟨left, right⟩ := (disjoint_join_right third first second).mp known.symm
    exact ⟨left.symm, right.symm⟩
  · rintro ⟨left, right⟩
    exact ((disjoint_join_right third first second).mpr ⟨left.symm, right.symm⟩).symm

end Resource
end Witchy.Logic

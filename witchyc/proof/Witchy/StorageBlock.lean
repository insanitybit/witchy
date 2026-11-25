import Init

namespace Witchy.Storage

/-- Typed slot storage shared by the executable scalar specialization and the
    generic container foundation. Initialization is separate from ownership.

    Representation: a block IS a Lean `Array` of cells. In the kernel `Array` is
    the one-field structure `Array.mk (toList : List _)`, so the logical model
    remains exactly the cell list (`Block.slots`), `⟨cells⟩` still constructs a
    block from a list, and every existing list-level statement keeps its
    meaning. The default compiled backend is Lean's contiguous object array. The separate NativeStorageBlock API supports type-directed packed native
    blocks without changing this interpreter-visible logical Array API. That
    physical mapping is trusted and tested. -/
abbrev Block (Value : Type) : Type := Array (Option Value)

/-- Logical constructor name retained for existing statements. Compiled code
    reaching it pays `lean_array_mk` (linear in the list); the Memory access
    paths never do. -/
@[match_pattern] abbrev Block.mk (slots : List (Option Value)) : Block Value := Array.mk slots

/-- Logical cell list. Deliberately not reducible, so the existing list-level
    simp normal forms are not rewritten into Array normal forms. Compiled code
    reaching it pays `lean_array_to_list`; Memory replaces each such use with a
    kernel-proved dense equivalent (`@[csimp]`). -/
def Block.slots (block : @& Block Value) : List (Option Value) := Array.toList block

@[simp] theorem Block.slots_mk (slots : List (Option Value)) :
    Block.slots (Array.mk slots : Block Value) = slots := rfl

/-- Bare `simp` rewrites a freshly allocated block's list into `Array.replicate`;
    this returns its logical cell list. -/
@[simp] theorem Block.slots_replicate (count : Nat) (cell : Option Value) :
    Block.slots (Array.replicate count cell : Block Value) = List.replicate count cell :=
  Array.toList_replicate

/-- Rebuilding a block from its own cell list is the block (structure eta). -/
theorem Block.mk_slots (block : Block Value) : (⟨block.slots⟩ : Block Value) = block := rfl

theorem Block.mk.inj {left right : List (Option Value)}
    (same : (Block.mk left : Block Value) = Block.mk right) : left = right := Array.mk.inj same

/-- Dense lookup agrees with the logical list lookup. -/
theorem Block.slots_getElem? (block : Block Value) (index : Nat) :
    block.slots[index]? = block[index]? := Array.getElem?_toList

theorem Block.slots_length (block : Block Value) : block.slots.length = block.size := rfl

theorem Block.slots_all (block : Block Value) (accepted : Option Value → Bool) :
    block.slots.all accepted = Array.all block accepted := Array.all_toList

/-- Dense single-cell replacement agrees with the logical list update. -/
theorem Block.mk_slots_set (block : Block Value) (index : Nat) (cell : Option Value) :
    (⟨block.slots.set index cell⟩ : Block Value) = block.setIfInBounds index cell := by
  apply Array.ext'
  simp [Block.slots]

/-- Equality is decided on the logical cell list, as the derived instance did. -/
instance [DecidableEq Value] : DecidableEq (Block Value) := fun left right =>
  decidable_of_iff (left.slots = right.slots)
    ⟨fun same => by cases left; cases right; exact congrArg Array.mk same, fun same => same ▸ rfl⟩

instance [Repr Value] : Repr (Block Value) where
  reprPrec block _ := Std.Format.bracket "{ " ("slots := " ++ repr block.slots) " }"

/-- Dense logical Array operations also usable by the Lean interpreter. The
    native packed adapter has separate, definitionally equal extern wrappers. -/
def Block.lookup (block : @& Block Value) (index : @& Nat) : Option (Option Value) := block[index]?

def Block.replace (block : Block Value) (index : @& Nat) (cell : Option Value) : Block Value :=
  block.setIfInBounds index cell

def Block.count (block : @& Block Value) : Nat := block.size

def Block.every (block : @& Block Value) (accepted : Option Value → Bool) : Bool :=
  Array.all block accepted

/-- Local cell transfer. A refused transfer retains the complete block; an
    initialized cell transfers its payload and becomes uninitialized. Memory
    access permission and allocation ownership are checked by the enclosing layer. -/
def Block.take (block : Block Value) (index : Nat) : Block Value × Option Value :=
  match block.slots[index]? with
  | some (some value) => (⟨block.slots.set index none⟩, some value)
  | _ => (block, none)

theorem Block.take_initialized (block : Block Value) (index : Nat) (value : Value)
    (found : block.slots[index]? = some (some value)) :
    block.take index = (⟨block.slots.set index none⟩, some value) := by
  simp [Block.take, found]

theorem Block.take_split (front back : List (Option Value)) (value : Value) :
    Block.take ⟨front ++ some value :: back⟩ front.length = (⟨front ++ none :: back⟩, some value) := by
  induction front with
  | nil => rfl
  | cons head tail _ih => simp [Block.take]

/-- Failure does not consume or overwrite a payload. -/
theorem Block.take_refused (block : Block Value) (index : Nat)
    (missing : block.slots[index]? = none ∨ block.slots[index]? = some none) :
    block.take index = (block, none) := by
  rcases missing with found | found <;> simp [Block.take, found]

/-- Dense logical take preserves Array storage as well as clearing
    initialization; its logical definition is the existing cell transfer. -/
def Block.takeDense (block : Block Value) (index : Nat) : Block Value × Option Value :=
  match Block.lookup block index with
  | some (some value) => (Block.replace block index none, some value)
  | _ => (block, none)

@[csimp] theorem Block.take_eq_takeDense : @Block.take = @Block.takeDense := by
  funext Value block index
  simp only [Block.take, Block.takeDense, Block.lookup, Block.replace,
    Block.slots_getElem?, Block.mk_slots_set]

end Witchy.Storage

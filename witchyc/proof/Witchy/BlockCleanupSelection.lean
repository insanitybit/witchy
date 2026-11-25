import Witchy.StorageBlock
import Witchy.Inventory

namespace Witchy.Storage

/-- Deterministic last-to-first selection skips uninitialized capacity cells. -/
private def lastInitializedSlot : List (Option Value) → Option (Nat × Value)
  | [] => none
  | head :: tail =>
    match lastInitializedSlot tail with
    | some (index, value) => some (index + 1, value)
    | none => head.map fun value => (0, value)

def Block.lastInitialized (block : Block Value) : Option (Nat × Value) := lastInitializedSlot block.slots

private theorem selected_slot (slots : List (Option Value)) (index : Nat) (value : Value)
    (selected : lastInitializedSlot slots = some (index, value)) : slots[index]? = some (some value) := by
  induction slots generalizing index value with
  | nil => simp [lastInitializedSlot] at selected
  | cons head tail ih =>
    cases last : lastInitializedSlot tail with
    | none =>
      cases head with
      | none => simp [lastInitializedSlot, last] at selected
      | some head =>
        have same : 0 = index ∧ head = value := by simpa [lastInitializedSlot, last] using selected
        rcases same with ⟨rfl, rfl⟩
        rfl
    | some pair =>
      obtain ⟨next, item⟩ := pair
      have same : next + 1 = index ∧ item = value := by simpa [lastInitializedSlot, last] using selected
      rcases same with ⟨rfl, rfl⟩
      simpa using ih next item last

private theorem no_slot (slots : List (Option Value)) :
    lastInitializedSlot slots = none ↔ ∀ cell ∈ slots, cell = none := by
  induction slots with
  | nil => simp [lastInitializedSlot]
  | cons head tail ih =>
    cases last : lastInitializedSlot tail with
    | none =>
      cases head <;> simp_all [lastInitializedSlot]
      exact ih
    | some pair => cases pair; simp_all [lastInitializedSlot]

/-- A selected cell is physically present and initialized; no successful take
    is assumed. This works for arbitrary payload types and initialization holes. -/
theorem Block.last_initialized_present (block : Block Value) (index : Nat) (value : Value)
    (selected : block.lastInitialized = some (index, value)) : block.slots[index]? = some (some value) :=
  selected_slot block.slots index value selected

/-- No selected child proves the entire allocation is empty and can proceed to
    checked backing release once permission/ownership obligations are met. -/
theorem Block.last_initialized_none (block : Block Value) :
    block.lastInitialized = none ↔ ∀ cell ∈ block.slots, cell = none := no_slot block.slots

def Block.initializedCount (block : Block Value) : Nat :=
  (block.slots.map fun cell => if cell.isSome then 1 else 0).sum

theorem Block.initialized_count_empty (block : Block Value)
    (empty : ∀ cell ∈ block.slots, cell = none) : block.initializedCount = 0 := by
  apply List.sum_eq_zero_iff_forall_eq_nat.mpr
  intro count member
  obtain ⟨cell, present, rfl⟩ := List.mem_map.mp member
  rw [empty cell present]
  rfl

/-- Clearing an initialized cell removes exactly one pending heap cell. -/
theorem Block.clear_initialized_progress (block : Block Value) (index : Nat) (value : Value)
    (initialized : block.slots[index]? = some (some value)) :
    Block.initializedCount (⟨block.slots.set index none⟩ : Block Value) + 1 = block.initializedCount := by
  have count := sum_set_weight block.slots index (some value) none
    (fun cell => if cell.isSome then 1 else 0) initialized
  simpa [Block.initializedCount] using count

end Witchy.Storage

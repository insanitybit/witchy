import Witchy.MemoryProofs

namespace Witchy.Memory

/-- With an explicit disposition predicate, every removed initialized cell
    must satisfy that predicate; callers must prove it carries no live duty. -/
theorem release_checked_safe (s : StateOf Value) (handle : Handle) (discardable : Value → Bool)
    (accepted : (releaseEmpty s handle discardable).2 = .ok ()) :
    ∃ block, block? s handle = some block ∧ (active s handle).isEmpty = true ∧
      (∀ cell ∈ block.slots, cell.elim true discardable = true) ∧
      (releaseEmpty s handle discardable).1 =
        { s with blocks := s.blocks.set handle none, available := s.available + block.slots.length } := by
  unfold releaseEmpty at accepted ⊢
  split at accepted
  · cases accepted
  · rename_i block found
    split at accepted
    · rename_i free
      split at accepted
      · rename_i trivial
        refine ⟨block, found, free, List.all_eq_true.mp trivial, ?_⟩
        simp only [free, trivial, ↓reduceIte]
      · cases accepted
    · cases accepted

/-- Successful generic release proves no initialized payload is discarded and
    no active loan survives, and identifies the exact storage refund/update. -/
theorem release_empty_safe (s : StateOf Value) (handle : Handle)
    (accepted : (releaseEmpty s handle).2 = .ok ()) :
    ∃ block, block? s handle = some block ∧ (active s handle).isEmpty = true ∧
      (∀ cell ∈ block.slots, cell = none) ∧
      (releaseEmpty s handle).1 =
        { s with blocks := s.blocks.set handle none, available := s.available + block.slots.length } := by
  cases found : block? s handle with
  | none => simp [releaseEmpty, found] at accepted
  | some block =>
    by_cases free : (active s handle).isEmpty = true
    · by_cases empty : block.slots.all Option.isNone = true
      · refine ⟨block, rfl, free, ?_, ?_⟩
        · intro cell member
          exact Option.isNone_iff_eq_none.mp ((List.all_eq_true.mp empty) cell member)
        · simp [releaseEmpty, found, free, empty]
      · simp [releaseEmpty, found, free, empty] at accepted
    · simp [releaseEmpty, found, free] at accepted

theorem release_empty_refused (s : StateOf Value) (handle : Handle) (reason : Error)
    (refused : (releaseEmpty s handle).2 = .error reason) : (releaseEmpty s handle).1 = s := by
  cases found : block? s handle with
  | none => simp [releaseEmpty, found]
  | some block =>
    by_cases free : (active s handle).isEmpty = true
    · by_cases empty : block.slots.all Option.isNone = true
      · simp [releaseEmpty, found, free, empty] at refused
      · simp [releaseEmpty, found, free, empty]
    · simp [releaseEmpty, found, free]

/-- An initialized owning payload cannot be discarded by generic release. -/
theorem release_empty_initialized (s : StateOf Value) (handle : Handle) (block : Storage.Block Value)
    (value : Value) (found : block? s handle = some block) (present : some value ∈ block.slots)
    (free : (active s handle).isEmpty = true) : releaseEmpty s handle = (s, .error .initialized) := by
  have occupied : ¬ block.slots.all Option.isNone = true := by
    intro empty
    have impossible := (List.all_eq_true.mp empty) (some value) present
    simp at impossible
  simp [releaseEmpty, found, free, occupied]

end Witchy.Memory

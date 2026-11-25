import Witchy.MemoryContents

namespace Witchy.Memory

/-- Taking returns the actual initialized value and clears exactly its cell. -/
theorem take_cell_effect (s next : StateOf Value) (access : Access) (index : Nat) (value : Value)
    (accepted : take s access index = (next, .ok value)) :
    ∃ handle block, resolve s access true = .ok handle ∧ block? s handle = some block ∧
      block.slots[index]? = some (some value) ∧
      next = { s with blocks := s.blocks.set handle (some ⟨block.slots.set index none⟩) } := by
  unfold take at accepted
  split at accepted
  · cases accepted
  · rename_i handle permission
    split at accepted
    · cases accepted
    · rename_i block found
      split at accepted
      · cases accepted
      · cases accepted
      · rename_i actual cell
        cases accepted
        exact ⟨handle, block, permission, found, cell, rfl⟩

/-- Reinitializing the cell just taken restores the complete memory state. -/
theorem take_write_restores (s next : State) (access : Access) (index : Nat) (value : UInt64)
    (accepted : take s access index = (next, .ok value)) :
    writeEmpty next access index value = (s, .ok ()) := by
  obtain ⟨handle, block, permission, found, cell, effect⟩ :=
    take_cell_effect s next access index value accepted
  have indexBound := (List.getElem?_eq_some_iff.mp cell).1
  have resolution : resolve next access true = .ok handle := by
    have frame := take_preserves_resolution s access access index true
    rw [accepted] at frame
    exact frame.trans permission
  subst next
  have changed := block_after_set s handle block ⟨block.slots.set index none⟩ found
  simp only [writeEmpty, resolution, changed, List.getElem?_set_self indexBound, Storage.Block.slots_mk]
  have restored : (block.slots.set index none).set index (some value) = block.slots := by
    rw [List.set_set]
    have actual := (List.getElem?_eq_some_iff.mp cell).2
    rw [← actual]
    exact List.set_getElem_self indexBound
  rw [restored, List.set_set]
  have original := block_lookup s handle block found
  have bound := (List.getElem?_eq_some_iff.mp original).1
  have actual := (List.getElem?_eq_some_iff.mp original).2
  have restoredBlocks : s.blocks.set handle (some block) = s.blocks := by
    rw [← actual]
    exact List.set_getElem_self bound
  simp only [Storage.Block.mk_slots, restoredBlocks]

/-- A second take through the same access refuses the consumed cell. -/
theorem take_again_uninitialized (s next : State) (access : Access) (index : Nat) (value : UInt64)
    (accepted : take s access index = (next, .ok value)) :
    take next access index = (next, .error .uninitialized) := by
  obtain ⟨handle, block, permission, found, cell, effect⟩ :=
    take_cell_effect s next access index value accepted
  have bound := (List.getElem?_eq_some_iff.mp cell).1
  have frame := take_preserves_resolution s access access index true
  rw [accepted] at frame
  have resolution := frame.trans permission
  subst next
  have changed := block_after_set s handle block ⟨block.slots.set index none⟩ found
  simp only [take, resolution, changed, List.getElem?_set_self bound, Storage.Block.slots_mk]

theorem read_resolution_of_write (s : State) (access : Access) (handle : Handle)
    (writable : resolve s access true = .ok handle) : resolve s access false = .ok handle := by
  cases access <;> simp only [resolve] at *
  all_goals repeat' first | assumption | split at * | simp_all

/-- A successful take also removes the value from subsequent reads. -/
theorem read_after_take_uninitialized (s next : State) (access : Access) (index : Nat) (value : UInt64)
    (accepted : take s access index = (next, .ok value)) :
    read next access index = .error .uninitialized := by
  obtain ⟨handle, block, permission, found, cell, effect⟩ :=
    take_cell_effect s next access index value accepted
  have bound := (List.getElem?_eq_some_iff.mp cell).1
  have frame := take_preserves_resolution s access access index false
  rw [accepted] at frame
  have resolution := frame.trans (read_resolution_of_write s access handle permission)
  subst next
  have changed := block_after_set s handle block ⟨block.slots.set index none⟩ found
  simp only [read, resolution, changed, List.getElem?_set_self bound, Storage.Block.slots_mk]

end Witchy.Memory

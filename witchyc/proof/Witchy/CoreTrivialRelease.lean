import Witchy.HeapResourceRelease

namespace Witchy.Storage

private theorem trivial_release_zero (cells : List (Option Core.Value)) (resource : Core.Resource)
    (trivial : ∀ cell ∈ cells, cell.elim true Core.Value.trivialRelease = true) :
    Core.registerCount cells resource = 0 := by
  induction cells with
  | nil => rfl
  | cons head tail ih =>
    have allowed := trivial head (by simp)
    have rest := ih (fun cell member => trivial cell (by simp [member]))
    have zero : Core.weight resource head = 0 := by
      cases head with
      | none => simp [Core.weight]
      | some value => cases value <;> simp_all [Core.Value.trivialRelease, Core.weight, Core.valueResource]
    change Core.weight resource head + Core.registerCount tail resource = 0
    rw [zero, rest]

/-- Resource-valued Core release may discard trivial values but never an owner
    or loan. Nested resource inventory therefore remains exactly unchanged. -/
theorem core_trivial_release_inventory (memory : Memory.StateOf Core.Value) (handle : Nat)
    (accepted : (Memory.releaseEmpty memory handle Core.Value.trivialRelease).2 = .ok ())
    (resource : Core.Resource) :
    heapResources (Memory.releaseEmpty memory handle Core.Value.trivialRelease).1 resource =
      heapResources memory resource := by
  obtain ⟨block, found, _, trivial, effect⟩ :=
    Memory.release_checked_safe memory handle Core.Value.trivialRelease accepted
  have zero := trivial_release_zero block.slots resource trivial
  have inventory := sum_set_weight memory.blocks handle (some block) none (blockResources resource)
    (Memory.block_lookup memory handle block found)
  rw [effect]
  change ((memory.blocks.set handle none).map (blockResources resource)).sum = _
  simpa [heapResources, blockResources, zero] using inventory

theorem core_trivial_release_live (memory : Memory.StateOf Core.Value) (handle : Memory.Handle)
    (accepted : (Memory.releaseEmpty memory handle Core.Value.trivialRelease).2 = .ok ()) (resource : Core.Resource) :
    Core.memoryCount (Memory.releaseEmpty memory handle Core.Value.trivialRelease).1 resource + Core.weight resource (some (.owner handle)) =
      Core.memoryCount memory resource := by
  obtain ⟨block, found, _free, _empty, effect⟩ := Memory.release_checked_safe memory handle Core.Value.trivialRelease accepted
  cases resource with
  | loan identity =>
    rw [effect]
    simp [Core.memoryCount, Memory.loan?, Core.weight, Core.valueResource]
  | allocation other =>
    by_cases same : handle = other
    · subst other
      have bound := (List.getElem?_eq_some_iff.mp (Memory.block_lookup memory handle block found)).1
      have gone : Memory.block? (Memory.releaseEmpty memory handle Core.Value.trivialRelease).1 handle = none := by
        rw [effect]
        simp [Memory.block?, List.getElem?_set_self bound]
      simp [Core.memoryCount, gone, found, Core.weight, Core.valueResource]
    · have unchanged : Memory.block? (Memory.releaseEmpty memory handle Core.Value.trivialRelease).1 other = Memory.block? memory other := by
        rw [effect]
        simp only [Memory.block?, List.getElem?_set_ne same]
      simp [Core.memoryCount, unchanged, Core.weight, Core.valueResource, same]

/-- Releasing trivial payloads consumes exactly the registered allocation owner. -/
theorem core_trivial_release_owned (registers : List (Option Core.Value)) (memory : Memory.StateOf Core.Value)
    (source handle : Nat) (owned : OwnsHeapResources registers memory)
    (present : registers[source]? = some (some (.owner handle)))
    (accepted : (Memory.releaseEmpty memory handle Core.Value.trivialRelease).2 = .ok ()) :
    OwnsHeapResources (registers.set source none)
      (Memory.releaseEmpty memory handle Core.Value.trivialRelease).1 := by
  intro resource
  have prior := owned resource
  have heap := core_trivial_release_inventory memory handle accepted resource
  have live := core_trivial_release_live memory handle accepted resource
  have cleared := Core.register_set_accounting registers source (some (.owner handle)) none resource present
  have zero : Core.weight resource none = 0 := by simp [Core.weight]
  rw [zero] at cleared
  rw [heap]
  omega

end Witchy.Storage

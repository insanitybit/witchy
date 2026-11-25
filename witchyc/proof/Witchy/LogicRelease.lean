import Witchy.LogicServiceFacts

namespace Witchy.Logic

structure ReleaseInput (resource : Resource) (source handle size available slots limit issued : Nat) : Prop where
  owner : resource (.register source) = some (some (.owner handle))
  storage : resource (.block handle) = some ⟨List.replicate size none⟩
  permission : resource (.permissions handle) = some []
  service : resource .allocator = some (available, slots, limit, issued)

def ReleaseWritten (source handle : Nat) : List Location :=
  [.register source, .block handle, .permissions handle, .allocator]

def ReleaseOutput (source handle size available slots limit issued : Nat) (before : Resource)
    (result : Core.Result Unit) (after : Resource) : Prop :=
  result = .ok () ∧ after (.register source) = some none ∧ after (.block handle) = none ∧
    after (.permissions handle) = none ∧ after .allocator = some (available + size, slots, limit, issued) ∧
    Resource.SameOutside (ReleaseWritten source handle) before after

/-- Releasing fully uninitialized storage refunds cells without reusing the
    allocation identity. No payload, loan, or capability can be discarded. -/
theorem release_rule (source handle size available slots limit issued : Nat) :
    AtomicRule (.release source)
      (fun resource => ReleaseInput resource source handle size available slots limit issued)
      (ReleaseOutput source handle size available slots limit issued) := by
  apply AtomicRule.of_effect
  intro resource store input _valid known
  have owner := known (.register source) _ input.owner
  change store.registers[source]? = some (some (.owner handle)) at owner
  have block := known (.block handle) _ input.storage
  change Memory.block? store.memory handle = some ⟨List.replicate size none⟩ at block
  have permissions := known (.permissions handle) _ input.permission
  have free := permissions_observed store handle [] permissions
  have supply := allocator_observed store resource available slots limit issued known input.service
  have registerBound := (List.getElem?_eq_some_iff.mp owner).1
  have blockBound := (List.getElem?_eq_some_iff.mp (Memory.block_lookup store.memory handle _ block)).1
  let next : Core.StateOf Core.Value :=
    { store with
      registers := store.registers.set source none
      memory := { store.memory with blocks := store.memory.blocks.set handle none, available := store.memory.available + size } }
  have released : Memory.releaseEmpty store.memory handle Core.Value.trivialRelease =
      (next.memory, .ok ()) := by
    simp [Memory.releaseEmpty, block, free, next]
  have performed : Core.step store (.release source) = .ok next := by
    simp [Core.step, Core.get, owner, Core.StorageCell.release, released, Core.fromMemory, next, Bind.bind, Pure.pure]
  have removed : Memory.block? next.memory handle = none := by
    change ((store.memory.blocks.set handle none)[handle]?).bind id = none
    rw [List.getElem?_set_self blockBound]
    rfl
  rw [performed]
  refine ⟨ReleaseWritten source handle, ?_, ?_, rfl, ?_, ?_, ?_, ?_, Resource.patch_same_outside _ _ _⟩
  · intro location member
    simp only [ReleaseWritten, List.mem_cons, List.not_mem_nil, or_false] at member
    rcases member with rfl | rfl | rfl | rfl
    · exact .inl (Resource.occupied input.owner)
    · exact .inl (Resource.occupied input.storage)
    · exact .inl (Resource.occupied input.permission)
    · exact .inl (Resource.occupied input.service)
  · intro location outside
    cases location with
    | register index =>
      have different : source ≠ index := by intro same; subst index; simp [ReleaseWritten] at outside
      exact List.getElem?_set_ne different
    | block other =>
      have different : handle ≠ other := by intro same; subst other; simp [ReleaseWritten] at outside
      exact congrArg (fun cell : Option (Option (Storage.Block Core.Value)) => cell.bind id) (List.getElem?_set_ne different)
    | loan _ => rfl
    | permissions other =>
      have different : handle ≠ other := by intro same; subst other; simp [ReleaseWritten] at outside
      exact congrArg (fun entry => entry.map (fun _ => Memory.active store.memory other))
        (congrArg (fun cell : Option (Option (Storage.Block Core.Value)) => cell.bind id) (List.getElem?_set_ne different))
    | allocator => simp [ReleaseWritten] at outside
    | loanSupply | authority => rfl
  · exact (Resource.patch_at _ _ _ _ (by simp [ReleaseWritten])).trans (List.getElem?_set_self registerBound)
  · exact (Resource.patch_at _ _ _ _ (by simp [ReleaseWritten])).trans removed
  · apply (Resource.patch_at _ _ _ _ (by simp [ReleaseWritten])).trans
    exact absent_permission next handle removed
  · apply (Resource.patch_at _ _ _ _ (by simp [ReleaseWritten])).trans
    change some (store.memory.available + size, store.memory.limits.slots, store.memory.limits.allocations,
      (store.memory.blocks.set handle none).length) = _
    simp only [List.length_set, supply.1, supply.2.1, supply.2.2.1, supply.2.2.2]

end Witchy.Logic

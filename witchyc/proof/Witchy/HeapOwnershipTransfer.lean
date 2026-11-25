import Witchy.HeapOwnershipGraph
import Witchy.HeapResourceTransfer

namespace Witchy.Storage

/-- Replacing a block cannot add an ownership edge except one present in the
    replacement itself. This frames every other allocation by identity. -/
theorem replacement_ownership_edge (memory : Memory.StateOf Core.Value)
    (handle : Memory.Handle) (old replacement : Block Core.Value)
    (found : Memory.block? memory handle = some old) (child parent : Memory.Handle)
    (edge : OwnsChild { memory with blocks := memory.blocks.set handle (some replacement) } child parent) :
    OwnsChild memory child parent ∨ (parent = handle ∧ some (.owner child) ∈ replacement.slots) := by
  obtain ⟨stored, lookup, member⟩ := edge
  by_cases same : handle = parent
  · subst parent
    rw [Memory.block_after_set memory handle old replacement found] at lookup
    cases lookup
    exact .inr ⟨rfl, member⟩
  · exact .inl ⟨stored, by simpa only [Memory.block?, List.getElem?_set_ne same] using lookup, member⟩

theorem take_ownership_wellFounded (memory : Memory.StateOf Core.Value) (access : Memory.Access)
    (index : Nat) (value : Core.Value) (valid : OwnershipWellFounded memory)
    (accepted : (Memory.take memory access index).2 = .ok value) :
    OwnershipWellFounded (Memory.take memory access index).1 := by
  let next := (Memory.take memory access index).1
  have complete : Memory.take memory access index = (next, .ok value) := by rw [← accepted]
  obtain ⟨handle, block, _, found, _, effect⟩ := Memory.take_cell_effect memory next access index value complete
  apply ownership_subrelation memory _ valid
  intro child parent edge
  change OwnsChild next child parent at edge
  rw [effect] at edge
  rcases replacement_ownership_edge memory handle block ⟨block.slots.set index none⟩ found child parent edge with prior | ⟨same, member⟩
  · exact prior
  · subst parent
    rcases List.mem_or_eq_of_mem_set member with prior | impossible
    · exact ⟨block, found, prior⟩
    · cases impossible

/-- A registered allocation owner cannot simultaneously be owned by a heap
    payload. This follows from combined inventory, not from handle numbering. -/
theorem registered_owner_is_root (registers : List (Option Core.Value))
    (memory : Memory.StateOf Core.Value) (source handle : Nat)
    (owned : OwnsHeapResources registers memory)
    (present : registers[source]? = some (some (.owner handle))) :
    ∀ parent, ¬ OwnsChild memory handle parent := by
  have positive := Core.present_resource_count_positive registers source (.owner handle)
    (.allocation handle) present rfl
  have bounded := owned_heap_at_most_one registers memory owned (.allocation handle)
  have empty : heapResources memory (.allocation handle) = 0 := by omega
  intro parent edge
  obtain ⟨block, found, member⟩ := edge
  have blockMember := List.mem_of_getElem? (Memory.block_lookup memory parent block found)
  have counted := (List.sum_eq_zero_iff_forall_eq_nat.mp empty)
    (blockResources (.allocation handle) (some block)) (List.mem_map.mpr ⟨some block, blockMember, rfl⟩)
  obtain ⟨index, cell⟩ := List.mem_iff_getElem?.mp member
  have cellPositive := Core.present_resource_count_positive block.slots index (.owner handle)
    (.allocation handle) cell rfl
  simp only [blockResources, Option.map_some, Option.getD_some] at counted
  omega

/-- Attaching children to an existing root preserves well-foundedness when
    none of the new children is that root. No ordering of allocation IDs is
    assumed; in particular an older allocation may own a newer allocation. -/
theorem root_insertion_wellFounded (before after : Memory.StateOf Core.Value)
    (root : Memory.Handle) (valid : OwnershipWellFounded before)
    (isRoot : ∀ parent, ¬ OwnsChild before root parent)
    (edges : ∀ child parent, OwnsChild after child parent →
      OwnsChild before child parent ∨ (parent = root ∧ child ≠ root)) :
    OwnershipWellFounded after := by
  have away : ∀ node, Acc (OwnsChild before) node → node ≠ root → Acc (OwnsChild after) node := by
    intro node accessible
    induction accessible with
    | intro node _ ih =>
      intro different
      apply Acc.intro
      intro child edge
      rcases edges child node edge with prior | ⟨same, _⟩
      · apply ih child prior
        intro same
        subst child
        exact isRoot node prior
      · exact False.elim (different same)
  refine ⟨fun node => ?_⟩
  by_cases same : node = root
  · subst node
    apply Acc.intro
    intro child edge
    apply away child (valid.apply child)
    rcases edges child root edge with prior | ⟨_, different⟩
    · intro same
      subst child
      exact isRoot root prior
    · exact different
  · exact away node (valid.apply node) same

/-- Actual initialization into a registered root is cycle-safe if it does not
    store that root's own owner. Resource/lifetime and relocation requirements
    remain separate: this theorem establishes only the ownership graph. -/
theorem write_root_ownership_wellFounded (registers : List (Option Core.Value))
    (memory : Memory.StateOf Core.Value) (access : Memory.Access) (index source root : Nat)
    (value : Core.Value) (owned : OwnsHeapResources registers memory)
    (present : registers[source]? = some (some (.owner root)))
    (permission : Memory.resolve memory access true = .ok root)
    (different : value ≠ .owner root) (valid : OwnershipWellFounded memory)
    (accepted : (Memory.writeEmpty memory access index value).2 = .ok ()) :
    OwnershipWellFounded (Memory.writeEmpty memory access index value).1 := by
  let next := (Memory.writeEmpty memory access index value).1
  have complete : Memory.writeEmpty memory access index value = (next, .ok ()) := by rw [← accepted]
  obtain ⟨handle, block, resolved, found, _, _, effect⟩ :=
    memory_write_buffer_push memory next access index value complete
  rw [permission] at resolved
  cases resolved
  apply root_insertion_wellFounded memory _ root valid
    (registered_owner_is_root registers memory source root owned present)
  intro child parent edge
  change OwnsChild next child parent at edge
  rw [effect] at edge
  rcases replacement_ownership_edge memory root block ⟨block.slots.set index (some value)⟩ found child parent edge with prior | ⟨same, member⟩
  · exact .inl prior
  · rcases List.mem_or_eq_of_mem_set member with prior | inserted
    · exact .inl ⟨block, by simpa [same] using found, prior⟩
    · apply Or.inr
      refine ⟨same, ?_⟩
      intro equal
      subst child
      exact different (Option.some.inj inserted).symm

/-- Consuming extraction to a vacant register preserves both the exact
    resource census and the structural condition for nested destruction. -/
theorem take_register_ownership_contract (registers : List (Option Core.Value))
    (memory : Memory.StateOf Core.Value) (access : Memory.Access) (index destination : Nat)
    (value : Core.Value) (owned : OwnsHeapResources registers memory)
    (valid : OwnershipWellFounded memory) (vacant : registers[destination]? = some none)
    (accepted : (Memory.take memory access index).2 = .ok value) :
    OwnsHeapResources (registers.set destination (some value)) (Memory.take memory access index).1 ∧
      OwnershipWellFounded (Memory.take memory access index).1 :=
  ⟨take_to_register_owns registers memory access index destination value owned vacant accepted,
    take_ownership_wellFounded memory access index value valid accepted⟩

/-- The source payload is consumed only by the paired successful store rule.
    A registered receiver root and the no-self-store obligation supply the
    cycle argument, while source inventory supplies ownership conservation. -/
theorem write_register_root_contract (registers : List (Option Core.Value))
    (memory : Memory.StateOf Core.Value) (access : Memory.Access) (index source receiver root : Nat)
    (value : Core.Value) (owned : OwnsHeapResources registers memory)
    (valid : OwnershipWellFounded memory)
    (payload : registers[source]? = some (some value))
    (present : registers[receiver]? = some (some (.owner root)))
    (permission : Memory.resolve memory access true = .ok root)
    (different : value ≠ .owner root)
    (accepted : (Memory.writeEmpty memory access index value).2 = .ok ()) :
    OwnsHeapResources (registers.set source none) (Memory.writeEmpty memory access index value).1 ∧
      OwnershipWellFounded (Memory.writeEmpty memory access index value).1 :=
  ⟨write_from_register_owns registers memory access index source value owned payload accepted,
    write_root_ownership_wellFounded registers memory access index receiver root value
      owned present permission different valid accepted⟩

end Witchy.Storage

import Witchy.LogicServiceFacts
import Witchy.LogicAllocate

namespace Witchy.Logic
open Core

structure AllocateInput (resource : Resource) (tag owner error count : Nat) (size : UInt64)
    (oldTag : Option Bool) (oldError : Option UInt64) (available slots limit issued : Nat) : Prop where
  separate : tag ≠ owner ∧ tag ≠ error ∧ owner ≠ error
  destination : resource (.register owner) = some none
  tagValue : resource (.register tag) = some (oldTag.map Value.boolean)
  errorValue : resource (.register error) = some (oldError.map Value.number)
  countValue : resource (.register count) = some (some (.number size))
  service : resource .allocator = some (available, slots, limit, issued)

def AllocateWritten (tag owner error issued : Nat) : List Location :=
  [.allocator, .block issued, .permissions issued, .register tag, .register owner, .register error]

def AllocateOutput (before : Resource) (tag owner error : Nat) (size : UInt64)
    (oldError : Option UInt64) (available slots limit issued : Nat)
    (result : Result Unit) (after : Resource) : Prop :=
  result = .ok () ∧ Resource.SameOutside (AllocateWritten tag owner error issued) before after ∧
    before (.block issued) = none ∧ before (.permissions issued) = none ∧
    if issued < limit ∧ size.toNat ≤ available then
      after (.register tag) = some (some (.boolean true)) ∧
      after (.register owner) = some (some (.owner issued)) ∧
      after (.register error) = some (oldError.map Value.number) ∧
      after (.block issued) = some ⟨List.replicate size.toNat none⟩ ∧
      after (.permissions issued) = some [] ∧
      after .allocator = some (available - size.toNat, slots, limit, issued + 1)
    else
      after (.register tag) = some (some (.boolean false)) ∧ after (.register owner) = some none ∧
      after (.register error) = some (some (.number (if issued < limit then 0 else 1))) ∧
      after (.block issued) = none ∧ after (.permissions issued) = none ∧
      after .allocator = some (available, slots, limit, issued)

theorem allocate_rule (tag owner error count : Nat) (size : UInt64)
    (oldTag : Option Bool) (oldError : Option UInt64) (available slots limit issued : Nat) :
    AtomicRule (.tryAllocate tag owner error count)
      (fun resource => AllocateInput resource tag owner error count size oldTag oldError available slots limit issued)
      (fun before => AllocateOutput before tag owner error size oldError available slots limit issued) := by
  apply AtomicRule.of_effect
  intro resource store input valid known
  have empty := known (.register owner) none input.destination
  change store.registers[owner]? = some none at empty
  have tagValue := known (.register tag) _ input.tagValue
  change store.registers[tag]? = some (oldTag.map Value.boolean) at tagValue
  have errorValue := known (.register error) _ input.errorValue
  change store.registers[error]? = some (oldError.map Value.number) at errorValue
  have position := known (.register count) _ input.countValue
  change store.registers[count]? = some (some (.number size)) at position
  have writable : vacant store owner = .ok () := by simp [vacant, empty]
  have tagSlot := bool_slot_observed store resource tag oldTag known input.tagValue
  have errorSlot := number_slot_observed store resource error oldError known input.errorValue
  have countValue : number store count = .ok size := by simp [number, Core.get, position, Bind.bind, Pure.pure]
  have supply := allocator_observed store resource available slots limit issued known input.service
  have futureBlock : observe store (.block issued) = none := by
    change Memory.block? store.memory issued = none
    rw [← supply.2.2.2]
    exact fresh_block_absent store
  have futurePermission := absent_permission store issued futureBlock
  have localBlock := agrees_absent store resource (.block issued) known futureBlock
  have localPermission := agrees_absent store resource (.permissions issued) known futurePermission
  obtain ⟨next, performed, effect, memory⟩ := allocate_locality store tag owner error count size
    input.separate writable tagSlot errorSlot countValue
  rw [supply.2.2.2] at effect
  rw [performed]
  refine ⟨AllocateWritten tag owner error issued, ?_, effect, rfl, Resource.patch_same_outside _ _ _, localBlock, localPermission, ?_⟩
  · intro location member
    simp only [AllocateWritten, List.mem_cons, List.not_mem_nil, or_false] at member
    rcases member with rfl | rfl | rfl | rfl | rfl | rfl
    · exact .inl (Resource.occupied input.service)
    · right
      change Memory.block? store.memory issued = none
      rw [← supply.2.2.2]
      exact fresh_block_absent store
    · right
      apply absent_permission
      rw [← supply.2.2.2]
      exact fresh_block_absent store
    · exact .inl (Resource.occupied input.tagValue)
    · exact .inl (Resource.occupied input.destination)
    · exact .inl (Resource.occupied input.errorValue)
  · by_cases fits : issued < limit ∧ size.toNat ≤ available
    · rw [ite_eq_left fits]
      have metadata : store.memory.blocks.length < store.memory.limits.allocations := by
        rw [supply.2.2.2, supply.2.2.1]
        exact fits.1
      have quota : size.toNat ≤ store.memory.available := by rw [supply.1]; exact fits.2
      have allocated : (Memory.allocate store.memory size.toNat).2 = .ok issued := by
        simp only [Memory.allocate, metadata, quota, ↓reduceIte]
        exact congrArg Memory.Result.ok supply.2.2.2
      have execution := try_allocate_success_effect_of store tag owner error count size issued
        input.separate writable tagSlot errorSlot countValue allocated
      have shape := Result.ok.inj (performed.symm.trans execution)
      have contents := (Memory.allocation_fresh store.memory size.toNat issued allocated).2
      have liveBlock : Memory.block? next.memory issued = some ⟨List.replicate size.toNat none⟩ := by
        rw [memory]
        exact contents
      refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
      · apply (Resource.patch_at _ _ _ _ (by simp [AllocateWritten])).trans
        change next.registers[tag]? = _
        rw [shape]
        simp [withBool, (List.getElem?_eq_some_iff.mp tagValue).1]
      · apply (Resource.patch_at _ _ _ _ (by simp [AllocateWritten])).trans
        change next.registers[owner]? = _
        rw [shape]
        simp only [withBool, List.getElem?_set_ne input.separate.1]
        exact List.getElem?_set_self (List.getElem?_eq_some_iff.mp empty).1
      · apply (Resource.patch_at _ _ _ _ (by simp [AllocateWritten])).trans
        change next.registers[error]? = _
        rw [shape]
        simpa only [withBool, List.getElem?_set_ne input.separate.2.1,
          List.getElem?_set_ne input.separate.2.2] using errorValue
      · exact (Resource.patch_at _ _ _ _ (by simp [AllocateWritten])).trans liveBlock
      · apply (Resource.patch_at _ _ _ _ (by simp [AllocateWritten])).trans
        change (Memory.block? next.memory issued).map (fun _ => Memory.active next.memory issued) = some []
        rw [liveBlock]
        have active : Memory.active next.memory issued = [] := by
          rw [memory, ← supply.2.2.2]
          have kept : Memory.active (Memory.allocate store.memory size.toNat).1 store.memory.blocks.length =
              Memory.active store.memory store.memory.blocks.length := by
            unfold Memory.allocate
            repeat' first | rfl | split
          rw [kept]
          exact fresh_permissions_free store valid
        rw [active]
        rfl
      · apply (Resource.patch_at _ _ _ _ (by simp [AllocateWritten])).trans
        change some (next.memory.available, next.memory.limits.slots,
          next.memory.limits.allocations, next.memory.blocks.length) = _
        rw [memory]
        simp only [Memory.allocate, metadata, quota, ↓reduceIte, List.length_append, List.length_singleton]
        rw [supply.1, supply.2.1, supply.2.2.1, supply.2.2.2]
    · rw [ite_eq_right fits]
      let reason : Memory.Error := if issued < limit then .allocation else .metadata
      have refusedState : Memory.allocate store.memory size.toNat = (store.memory, .error reason) := by
        unfold Memory.allocate
        rw [supply.1, supply.2.2.1, supply.2.2.2]
        by_cases metadata : issued < limit
        · have shortage : ¬ size.toNat ≤ available := fun quota => fits ⟨metadata, quota⟩
          simp [metadata, shortage, reason]
        · simp [metadata, reason]
      have code : (if reason = .allocation then (0 : UInt64) else 1) = (if issued < limit then 0 else 1) := by
        by_cases metadata : issued < limit <;> simp [reason, metadata]
      have execution := try_allocate_refusal_effect_of store tag owner error count size reason
        input.separate writable tagSlot errorSlot countValue (congrArg Prod.snd refusedState)
      rw [code] at execution
      have shape := Result.ok.inj (performed.symm.trans execution)
      have sameMemory : next.memory = store.memory := by rw [memory, refusedState]
      refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
      · apply (Resource.patch_at _ _ _ _ (by simp [AllocateWritten])).trans
        change next.registers[tag]? = _
        rw [shape]
        simp [withBool, withNumber, (List.getElem?_eq_some_iff.mp tagValue).1]
      · apply (Resource.patch_at _ _ _ _ (by simp [AllocateWritten])).trans
        change next.registers[owner]? = _
        rw [shape]
        simpa only [withBool, withNumber, List.getElem?_set_ne input.separate.1,
          List.getElem?_set_ne input.separate.2.2.symm] using empty
      · apply (Resource.patch_at _ _ _ _ (by simp [AllocateWritten])).trans
        change next.registers[error]? = _
        rw [shape]
        simp only [withBool, withNumber, List.getElem?_set_ne input.separate.2.1]
        exact List.getElem?_set_self (List.getElem?_eq_some_iff.mp errorValue).1
      · apply (Resource.patch_at _ _ _ _ (by simp [AllocateWritten])).trans
        change Memory.block? next.memory issued = none
        rw [sameMemory, ← supply.2.2.2]
        exact fresh_block_absent store
      · apply (Resource.patch_at _ _ _ _ (by simp [AllocateWritten])).trans
        apply absent_permission
        rw [sameMemory, ← supply.2.2.2]
        exact fresh_block_absent store
      · apply (Resource.patch_at _ _ _ _ (by simp [AllocateWritten])).trans
        change some (next.memory.available, next.memory.limits.slots,
          next.memory.limits.allocations, next.memory.blocks.length) = _
        rw [sameMemory, supply.1, supply.2.1, supply.2.2.1, supply.2.2.2]

end Witchy.Logic

import Witchy.LogicFootprint
import Witchy.CoreAllocationEffects
import Witchy.MemoryBlockFrame
import Witchy.RegisterWrites

namespace Witchy.Logic
open Core

private theorem allocation_observations (store next : StateOf Value) (size : Nat)
    (registers : List Nat)
    (memory : next.memory = (Memory.allocate store.memory size).1)
    (authority : next.authority = store.authority)
    (slots : ∀ index, index ∉ registers → next.registers[index]? = store.registers[index]?) :
    UnchangedOutside (.allocator :: .block store.memory.blocks.length ::
      .permissions store.memory.blocks.length :: registers.map Location.register) store next := by
  intro location outside
  cases location with
  | register index =>
    apply slots index
    intro member
    apply outside
    simp only [List.mem_cons]
    exact .inr (.inr (.inr (List.mem_map.mpr ⟨index, member, rfl⟩)))
  | block handle =>
    change Memory.block? next.memory handle = Memory.block? store.memory handle
    rw [memory]
    apply Memory.allocate_block_frame store.memory size handle
    simpa using outside
  | loan identity =>
    change Memory.loan? next.memory identity = Memory.loan? store.memory identity
    rw [memory]
    unfold Memory.allocate
    repeat' first | rfl | split
  | permissions handle =>
    have different : handle ≠ store.memory.blocks.length := by
      intro same
      subst handle
      simp at outside
    change (Memory.block? next.memory handle).map (fun _ => Memory.active next.memory handle) =
      (Memory.block? store.memory handle).map (fun _ => Memory.active store.memory handle)
    rw [memory]
    have blocks := Memory.allocate_block_frame store.memory size handle (by simpa using different)
    have active : Memory.active (Memory.allocate store.memory size).1 handle = Memory.active store.memory handle := by
      unfold Memory.allocate
      repeat' first | rfl | split
    rw [blocks, active]
  | allocator => simp at outside
  | loanSupply =>
    dsimp only [observe]
    rw [memory]
    unfold Memory.allocate
    repeat' first | rfl | split
  | authority => exact congrArg some authority

/-- Recoverable allocation is executable on either budget outcome; refusal
    preserves ownership. Only output slots, the fresh block and allocator
    service can change. No success/budget premise is required. -/
theorem allocate_locality (store : StateOf Value) (tag owner error count : Nat) (size : UInt64)
    (separate : tag ≠ owner ∧ tag ≠ error ∧ owner ≠ error)
    (empty : vacant store owner = .ok ()) (tagSlot : BoolSlot store tag)
    (errorSlot : NumberSlot store error) (countValue : number store count = .ok size) :
    ∃ next, step store (.tryAllocate tag owner error count) = .ok next ∧
      UnchangedOutside [.allocator, .block store.memory.blocks.length, .permissions store.memory.blocks.length,
        .register tag, .register owner, .register error] store next ∧
      next.memory = (Memory.allocate store.memory size.toNat).1 := by
  obtain ⟨next, performed⟩ := try_allocate_step_of store tag owner error count size
    separate empty tagSlot errorSlot countValue
  have effects : next.memory = (Memory.allocate store.memory size.toNat).1 ∧
      next.authority = store.authority := by
    cases outcome : (Memory.allocate store.memory size.toNat).2 with
    | ok handle =>
      have effect := try_allocate_success_effect_of store tag owner error count size handle
        separate empty tagSlot errorSlot countValue outcome
      have same := Result.ok.inj (performed.symm.trans effect)
      rw [same]
      exact ⟨rfl, rfl⟩
    | error reason =>
      have effect := try_allocate_refusal_effect_of store tag owner error count size reason
        separate empty tagSlot errorSlot countValue outcome
      have same := Result.ok.inj (performed.symm.trans effect)
      rw [same]
      exact ⟨(Memory.allocation_failure_atomic store.memory size.toNat reason outcome).symm, rfl⟩
  refine ⟨next, performed, ?_, effects.1⟩
  apply allocation_observations store next size.toNat [tag, owner, error] effects.1 effects.2
  intro index outside
  exact step_register_frame store next (.tryAllocate tag owner error count) index outside performed

end Witchy.Logic

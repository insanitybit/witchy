import Witchy.CoreCallRegisters
import Witchy.CoreCapabilityTransfer
import Witchy.CoreStorageExecution

namespace Witchy.Core.Call

private theorem count_append (first rest : List (Option Value)) (resource : Resource) :
    registerCount (first ++ rest) resource = registerCount first resource + registerCount rest resource := by
  simp only [registerCount, List.map_append, List.sum_append]

private theorem tally_append (first rest : List (Option Value)) (weight : Value → Nat) :
    Storage.payloadTally weight (first ++ rest) = Storage.payloadTally weight first + Storage.payloadTally weight rest := by
  simp only [Storage.payloadTally, List.map_append, List.sum_append]

private theorem vacant_count (count : Nat) (resource : Resource) :
    registerCount (List.replicate count none) resource = 0 := by
  simp [registerCount, weight]

private theorem vacant_tally (count : Nat) (weight : Value → Nat) :
    Storage.payloadTally weight (List.replicate count none) = 0 := by
  simp [Storage.payloadTally]

theorem open_inventory (s : StateOf Value) (count : Nat) (known : StorageStateInvariant s) :
    StorageStateInvariant (openRegisters s count) := by
  refine ⟨?_, known.structural, ⟨?_, known.capabilities.2⟩, ?_⟩
  · intro resource
    change registerCount (s.registers ++ List.replicate count none) resource + Storage.heapResources s.memory resource = memoryCount s.memory resource
    rw [count_append, vacant_count, Nat.add_zero]
    exact known.inventory resource
  · intro value member
    have member : some value ∈ s.registers ∨ some value ∈ List.replicate count none := List.mem_append.mp member
    rcases member with old | vacant
    · exact known.capabilities.1 value old
    · have impossible := List.mem_replicate.mp vacant
      cases impossible.2
  · intro loan member
    exact List.mem_append_left _ (known.roots loan member)

theorem open_capabilities (s : StateOf Value) (count : Nat) : CapabilityConserved s (openRegisters s count) := by
  refine ⟨rfl, ?_⟩
  intro token
  change Storage.payloadTally (Storage.capabilityWeight token) (s.registers ++ List.replicate count none) +
    Storage.heapPayloadTally (Storage.capabilityWeight token) s.memory = statePayloadTally (Storage.capabilityWeight token) s
  rw [tally_append, vacant_tally, Nat.add_zero]
  rfl

theorem open_keeps_caller (s : StateOf Value) (count index : Nat) (inside : index < s.registers.length) :
    (openRegisters s count).registers[index]? = s.registers[index]? := by
  simp only [openRegisters, List.getElem?_append_left inside]

private theorem scratch_count (cells : List (Option Value)) (clean : cells.all returnScratch = true)
    (resource : Resource) : registerCount cells resource = 0 := by
  induction cells with
  | nil => rfl
  | cons head tail ih =>
    have pair := Bool.and_eq_true_iff.mp clean
    have rest := ih pair.2
    cases head with
    | none => simpa [registerCount, weight, valueResource] using rest
    | some value => cases value <;> simp_all [returnScratch, registerCount, weight, valueResource]

private theorem scratch_tally (cells : List (Option Value)) (clean : cells.all returnScratch = true)
    (token : Nat) : Storage.payloadTally (Storage.capabilityWeight token) cells = 0 := by
  induction cells with
  | nil => rfl
  | cons head tail ih =>
    have pair := Bool.and_eq_true_iff.mp clean
    have rest := ih pair.2
    cases head with
    | none => simpa [Storage.payloadTally] using rest
    | some value => cases value <;> simp_all [returnScratch, Storage.payloadTally, Storage.capabilityWeight]

theorem close_inventory (s next : StateOf Value) (base : Nat)
    (accepted : closeRegisters s base = .ok next) (known : StorageStateInvariant s) : StorageStateInvariant next := by
  unfold closeRegisters at accepted
  split at accepted
  next allowed =>
    cases accepted
    obtain ⟨_bounded, clean⟩ := allowed
    have inventory : ∀ resource, registerCount (s.registers.take base) resource = registerCount s.registers resource := by
      intro resource
      have total : registerCount (s.registers.take base ++ s.registers.drop base) resource = registerCount s.registers resource := by rw [List.take_append_drop]
      simpa only [count_append, (scratch_count _ clean resource), Nat.add_zero] using total
    refine ⟨?_, known.structural, ⟨?_, known.capabilities.2⟩, ?_⟩
    · intro resource
      simpa only [inventory] using known.inventory resource
    · intro value member
      exact known.capabilities.1 value (List.mem_of_mem_take member)
    · intro loan member
      have original := known.roots loan member
      rw [← List.take_append_drop base s.registers] at original
      rcases List.mem_append.mp original with kept | lost
      · exact kept
      · have invalid := List.all_eq_true.mp clean _ lost
        cases invalid
  next => cases accepted

theorem close_capabilities (s next : StateOf Value) (base : Nat)
    (accepted : closeRegisters s base = .ok next) : CapabilityConserved s next := by
  unfold closeRegisters at accepted
  split at accepted
  next allowed =>
    cases accepted
    refine ⟨rfl, ?_⟩
    intro token
    have total : Storage.payloadTally (Storage.capabilityWeight token) (s.registers.take base ++ s.registers.drop base) =
        Storage.payloadTally (Storage.capabilityWeight token) s.registers := by rw [List.take_append_drop]
    have same : Storage.payloadTally (Storage.capabilityWeight token) (s.registers.take base) =
        Storage.payloadTally (Storage.capabilityWeight token) s.registers := by
      simpa only [tally_append, (scratch_tally _ allowed.2 token), Nat.add_zero] using total
    exact congrArg (fun count => count + Storage.heapPayloadTally (Storage.capabilityWeight token) s.memory) same
  next => cases accepted

end Witchy.Core.Call

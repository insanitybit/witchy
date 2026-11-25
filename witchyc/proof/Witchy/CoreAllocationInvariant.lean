import Witchy.HeapAllocationInvariant
import Witchy.HeapResourceAllocation
import Witchy.StorageStateInvariant
import Witchy.BorrowRootEffects
import Witchy.CoreScalarInvariant
import Witchy.ExecutionPath

namespace Witchy.Core

/-- Real allocation derives a fresh owning register and uninitialized block
    from the host budgets, retaining all operational storage obligations. -/
theorem allocate_storage_ready (s : StateOf Value) (dst count : Nat) (size : UInt64)
    (empty : s.registers[dst]? = some none) (position : number s count = .ok size)
    (metadata : s.memory.blocks.length < s.memory.limits.allocations)
    (budget : size.toNat ≤ s.memory.available) (invariant : StorageStateInvariant s) :
    ∃ next, step s (.allocate dst count) = .ok next ∧ StorageStateInvariant next ∧
      get next dst = .ok (.owner s.memory.blocks.length) ∧
      next.registers = s.registers.set dst (some (.owner s.memory.blocks.length)) ∧
      next.memory = (Memory.allocate s.memory size.toNat).1 ∧
      Memory.block? next.memory s.memory.blocks.length = some ⟨List.replicate size.toNat none⟩ ∧
      next.memory.available = s.memory.available - size.toNat ∧ next.authority = s.authority ∧
      (∀ program pc, program[pc]? = some (.allocate dst count) → Continues program 1 pc s (pc + 1) next) := by
  let memory := (Memory.allocate s.memory size.toNat).1
  have allocated : Memory.allocate s.memory size.toNat = (memory, .ok s.memory.blocks.length) := by
    simp [Memory.allocate, metadata, budget, memory]
  have accepted : (Memory.allocate s.memory size.toNat).2 = .ok s.memory.blocks.length := by rw [allocated]
  let next : StateOf Value := { s with memory, registers := s.registers.set dst (some (.owner s.memory.blocks.length)) }
  have performed : step s (.allocate dst count) = .ok next := by
    simp [step, vacant, empty, position, allocated, put, fromMemory, Bind.bind, Pure.pure, next]
  have owned := Storage.allocate_to_register_owns s.registers s.memory size.toNat s.memory.blocks.length dst
    invariant.inventory empty accepted
  have graph := Storage.allocation_ownership_wellFounded s.memory size.toNat invariant.structural
  have capabilities : Storage.allCapabilitiesValid next.authority next.registers next.memory := by
    constructor
    · exact Storage.cells_caps_set s.authority s.registers dst _ invariant.capabilities.1
        (by intro value same; cases same; trivial)
    · exact Storage.allocation_heap_capabilities s.authority s.memory size.toNat invariant.capabilities.2
  have roots := loan_roots_vacant s.registers memory.loans dst (some (.owner s.memory.blocks.length))
    (allocate_loan_roots s.registers s.memory size.toNat invariant.roots) empty
  refine ⟨next, performed, ⟨owned, graph, capabilities, roots⟩, ?_, rfl, rfl,
    (Memory.allocation_fresh s.memory size.toNat s.memory.blocks.length accepted).2, ?_, rfl, ?_⟩
  · simp [get, next, List.getElem?_set_self (List.getElem?_eq_some_iff.mp empty).1]
  · simp [next, memory, Memory.allocate, metadata, budget]
  · intro program pc code
    exact ExecutionEdge.continues program pc (pc + 1) s next (.instruction code performed)

end Witchy.Core

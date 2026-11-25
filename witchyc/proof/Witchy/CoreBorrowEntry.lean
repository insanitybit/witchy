import Witchy.HeapResourceLoans
import Witchy.CoreBorrowRoots
import Witchy.StorageStateInvariant
import Witchy.ExecutionPath

namespace Witchy.Core

/-- Inventory derives a live allocation from a registered owner. -/
theorem registered_owner_block (s : StateOf Value) (source handle : Nat)
    (owned : Storage.OwnsHeapResources s.registers s.memory)
    (present : s.registers[source]? = some (some (.owner handle))) :
    ∃ block, Memory.block? s.memory handle = some block := by
  have positive := present_resource_count_positive s.registers source (.owner handle) (.allocation handle) present rfl
  have inventory := owned (.allocation handle)
  cases found : Memory.block? s.memory handle with
  | none => simp [memoryCount, found] at inventory; omega
  | some block => exact ⟨block, rfl⟩

/-- Exclusive borrow entry derives the live target and actual loan creation
    from ownership, compatibility and metadata room. No borrow success premise. -/
theorem borrow_storage_ready (s : StateOf Value) (dst source handle : Nat)
    (present : s.registers[source]? = some (some (.owner handle)))
    (empty : s.registers[dst]? = some none) (free : (Memory.active s.memory handle).isEmpty = true)
    (room : s.memory.loans.length < s.memory.limits.loans)
    (invariant : StorageStateInvariant s) (valid : Memory.LoansValid s.memory) :
    ∃ next, step s (.borrow dst source true) = .ok next ∧ StorageStateInvariant next ∧
      Memory.LoansValid next.memory ∧ access next dst = .ok (.loan s.memory.nextLoan) ∧
      Memory.resolve next.memory (.loan s.memory.nextLoan) true = .ok handle ∧
      next.registers = s.registers.set dst (some (.reference s.memory.nextLoan)) ∧
      next.memory = (Memory.borrow s.memory handle true).1 ∧
      next.memory.blocks = s.memory.blocks ∧ next.memory.available = s.memory.available ∧
      next.authority = s.authority ∧
      (∀ program pc, program[pc]? = some (.borrow dst source true) → Continues program 1 pc s (pc + 1) next) := by
  obtain ⟨block, stored⟩ := registered_owner_block s source handle invariant.inventory present
  have rights : Memory.borrowable s.memory handle true = true := by simp [Memory.borrowable, free]
  let memory := (Memory.borrow s.memory handle true).1
  have borrowed : Memory.borrow s.memory handle true = (memory, .ok s.memory.nextLoan) := by
    simp [Memory.borrow, stored, room, rights, memory]
  have accepted : (Memory.borrow s.memory handle true).2 = .ok s.memory.nextLoan := by rw [borrowed]
  let next : StateOf Value := { s with memory, registers := s.registers.set dst (some (.reference s.memory.nextLoan)) }
  have performed : step s (.borrow dst source true) = .ok next := by
    simp [step, vacant, empty, get, present, borrowed, put, fromMemory, Bind.bind, Pure.pure, next]
  have owned := Storage.borrow_to_register_owns s.registers s.memory handle s.memory.nextLoan dst true
    invariant.inventory valid empty accepted
  have graph := Storage.borrow_ownership_wellFounded s.memory handle true invariant.structural
  have blocks : memory.blocks = s.memory.blocks := by simp [memory, Memory.borrow, stored, room, rights]
  have capabilities : Storage.allCapabilitiesValid next.authority next.registers next.memory := by
    constructor
    · exact Storage.cells_caps_set s.authority s.registers dst _ invariant.capabilities.1
        (by intro value same; cases same; trivial)
    · simpa only [Storage.heapCapabilitiesValid, next, blocks] using invariant.capabilities.2
  have roots := step_loan_roots s next (.borrow dst source true) invariant.roots performed
  refine ⟨next, performed, ⟨owned, graph, capabilities, roots⟩,
    Memory.borrow_loans_valid s.memory handle true valid, ?_, ?_, rfl, rfl, blocks, ?_, rfl, ?_⟩
  · simp [access, get, next, List.getElem?_set_self (List.getElem?_eq_some_iff.mp empty).1, Bind.bind, Pure.pure]
  · simp [next, memory, Memory.borrow, stored, room, rights, Memory.resolve, Memory.loan?]
  · simp [next, memory, Memory.borrow, stored, room, rights]
  · intro program pc code
    exact ExecutionEdge.continues program pc (pc + 1) s next (.instruction code performed)

end Witchy.Core

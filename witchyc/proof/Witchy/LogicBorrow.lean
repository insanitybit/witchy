import Witchy.LogicFootprint
import Witchy.LoanProofs

namespace Witchy.Logic
open Core

def borrowedMemory (memory : Memory.StateOf Value) (handle : Nat) (exclusive : Bool) : Memory.StateOf Value :=
  { memory with loans := ⟨memory.nextLoan, handle, exclusive⟩ :: memory.loans, nextLoan := memory.nextLoan + 1 }

theorem borrowed_permissions (memory : Memory.StateOf Value) (handle : Nat) (exclusive : Bool) :
    Memory.active (borrowedMemory memory handle exclusive) handle =
      ⟨memory.nextLoan, handle, exclusive⟩ :: Memory.active memory handle := by
  simp [borrowedMemory, Memory.active]

/-- A successful borrow writes only its destination, the fresh loan entry,
    the target's permission view and the loan service. -/
theorem borrow_success_locality (store : StateOf Value) (dst src handle : Nat)
    (exclusive : Bool) (block : Storage.Block Value)
    (empty : store.registers[dst]? = some none)
    (owner : store.registers[src]? = some (some (.owner handle)))
    (found : Memory.block? store.memory handle = some block)
    (rights : Memory.borrowable store.memory handle exclusive = true)
    (room : store.memory.loans.length < store.memory.limits.loans) :
    ∃ next, step store (.borrow dst src exclusive) = .ok next ∧
      UnchangedOutside [.register dst, .loan store.memory.nextLoan, .permissions handle, .loanSupply] store next ∧
      next.registers[dst]? = some (some (.reference store.memory.nextLoan)) ∧
      next.memory = borrowedMemory store.memory handle exclusive := by
  let loan : Memory.Loan := ⟨store.memory.nextLoan, handle, exclusive⟩
  let next : StateOf Value := { store with
    registers := store.registers.set dst (some (.reference store.memory.nextLoan))
    memory := { store.memory with loans := loan :: store.memory.loans, nextLoan := store.memory.nextLoan + 1 } }
  have performed : step store (.borrow dst src exclusive) = .ok next := by
    simp [step, vacant, Core.get, empty, owner, Memory.borrow, found, room, rights,
      fromMemory, put, next, loan, Bind.bind, Pure.pure]
  refine ⟨next, performed, ?_, ?_, rfl⟩
  · intro location outside
    cases location with
    | register index =>
      have different : dst ≠ index := by intro same; subst index; simp at outside
      exact List.getElem?_set_ne different
    | block other => rfl
    | loan identity =>
      have different : store.memory.nextLoan ≠ identity := by
        intro same
        rw [same] at outside
        simp at outside
      change (loan :: store.memory.loans).find? (fun entry => entry.identity == identity) =
        store.memory.loans.find? (fun entry => entry.identity == identity)
      have notNamed : (loan.identity == identity) = false := by simp [loan, different]
      simp only [List.find?_cons, notNamed]
    | permissions other =>
      have different : handle ≠ other := by intro same; subst other; simp at outside
      simp [observe, next, Memory.block?, Memory.active, loan, different] <;> rfl
    | allocator => rfl
    | loanSupply => simp at outside
    | authority => rfl
  · exact List.getElem?_set_self (List.getElem?_eq_some_iff.mp empty).1

/-- Exhaustion is an actual Core fault with no successful store result. It is
    not silently converted into a source Result or assumed away by a contract. -/
theorem borrow_exhaustion (store : StateOf Value) (dst src handle : Nat)
    (exclusive : Bool) (block : Storage.Block Value)
    (empty : store.registers[dst]? = some none)
    (owner : store.registers[src]? = some (some (.owner handle)))
    (found : Memory.block? store.memory handle = some block)
    (exhausted : ¬ store.memory.loans.length < store.memory.limits.loans) :
    step store (.borrow dst src exclusive) = .error (.memory .metadata) := by
  simp [step, vacant, Core.get, empty, owner, Memory.borrow, found, exhausted,
    fromMemory, Bind.bind]

/-- The primitive case split requires permissions and writable storage, but no
    loan-room premise. The recoverable library-return path remains separate. -/
theorem borrow_locality (store : StateOf Value) (dst src handle : Nat)
    (exclusive : Bool) (block : Storage.Block Value)
    (empty : store.registers[dst]? = some none)
    (owner : store.registers[src]? = some (some (.owner handle)))
    (found : Memory.block? store.memory handle = some block)
    (rights : Memory.borrowable store.memory handle exclusive = true) :
    if store.memory.loans.length < store.memory.limits.loans then
      ∃ next, step store (.borrow dst src exclusive) = .ok next ∧
        UnchangedOutside [.register dst, .loan store.memory.nextLoan, .permissions handle, .loanSupply] store next ∧
        next.registers[dst]? = some (some (.reference store.memory.nextLoan)) ∧
        next.memory = borrowedMemory store.memory handle exclusive
    else step store (.borrow dst src exclusive) = .error (.memory .metadata) := by
  split
  · exact borrow_success_locality store dst src handle exclusive block empty owner found rights ‹_›
  · exact borrow_exhaustion store dst src handle exclusive block empty owner found ‹_›

end Witchy.Logic

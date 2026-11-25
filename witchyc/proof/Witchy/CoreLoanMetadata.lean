import Witchy.CoreCellExecution
import Witchy.CoreComposition
import Witchy.LoanProofs

namespace Witchy.Memory

/-- Checked generic release changes storage, not loan metadata. -/
theorem release_empty_loan_property (predicate : List Loan → Nat → Nat → Prop)
    (s : StateOf Value) (handle : Handle) (discardable : Value → Bool) :
    LoanProperty predicate (releaseEmpty s handle discardable).1 ↔ LoanProperty predicate s := by
  unfold releaseEmpty
  repeat' first | rfl | split

end Witchy.Memory

namespace Witchy.Core

variable {Cell : Type} [StorageCell Cell]

/-- Register, allocation and scalar-memory operations cannot alter the loan
    table. Only the specified borrow/end operations change its live entries. -/
theorem step_loan_property_cells_guarded (predicate : List Memory.Loan → Nat → Nat → Prop)
    (s next : StateOf Cell) (instruction : Instruction)
    (hs : Memory.LoanProperty predicate s.memory)
    (hb : ∀ dst source h exclusive, instruction = .borrow dst source exclusive →
      Memory.LoanProperty predicate (Memory.borrow s.memory h exclusive).1)
    (he : ∀ key, Memory.LoanProperty predicate (Memory.endLoan s.memory key).1)
    (hr : ∀ handle, Memory.LoanProperty predicate (StorageCell.release s.memory handle).1)
    (h : step s instruction = .ok next) : Memory.LoanProperty predicate next.memory := by
  cases instruction
  case packMove dst sources widths =>
    rw [(pack_context s next dst sources widths h).1]
    exact hs
  case unpackMove destinations src widths =>
    rw [(unpack_context s next destinations src widths h).1]
    exact hs
  all_goals simp only [step, Bind.bind] at h
  case borrow dst src exclusive =>
    have current (handle : Memory.Handle) := hb dst src handle exclusive rfl
    simp only [Bind.bind, Pure.pure, put, fromMemory] at h
    repeat' first
      | (cases h <;> exact current _)
      | (split at * <;> try simp_all)
    all_goals (subst_vars; exact current _)
  case endLoan src =>
    simp only [Pure.pure, fromMemory] at h
    repeat' first
      | (cases h <;> exact he _)
      | (split at * <;> try simp_all)
    all_goals (subst_vars; exact he _)
  case release src =>
    simp only [Pure.pure, fromMemory] at h
    repeat' first
      | (cases h <;> exact hr _)
      | (split at * <;> try simp_all)
    all_goals (subst_vars; exact hr _)
  all_goals try simp only [Pure.pure, fromMemory, fromAuthority] at h
  all_goals repeat' first | split at h | cases h
  all_goals try exact hs
  all_goals grind only [→ put_memory, → copy_memory, Memory.allocate_loan_property,
    Memory.write_loan_property, Memory.take_loan_property]

theorem step_loan_property_cells (predicate : List Memory.Loan → Nat → Nat → Prop)
    (s next : StateOf Cell) (instruction : Instruction)
    (hs : Memory.LoanProperty predicate s.memory)
    (hb : ∀ h exclusive, Memory.LoanProperty predicate (Memory.borrow s.memory h exclusive).1)
    (he : ∀ key, Memory.LoanProperty predicate (Memory.endLoan s.memory key).1)
    (hr : ∀ handle, Memory.LoanProperty predicate (StorageCell.release s.memory handle).1)
    (h : step s instruction = .ok next) : Memory.LoanProperty predicate next.memory :=
  step_loan_property_cells_guarded predicate s next instruction hs (fun _ _ handle exclusive _ => hb handle exclusive) he hr h

/-- Resource-valued execution retains fresh, bounded and compatible loan tables.
    This statement is about the fixed Core.Value storage implementation. -/
theorem run_resource_loans_valid (program : Array Instruction) (fuel pc : Nat) (s : StateOf Value)
    (valid : Memory.LoansValid s.memory) : Memory.LoansValid (run program fuel pc s).1.memory := by
  apply run_preserves_cells (fun state => Memory.LoansValid state.memory) _ program fuel pc s valid
  intro state next instruction known accepted
  exact step_loan_property_cells Memory.LoanInvariant state next instruction known
    (fun handle exclusive => Memory.borrow_loans_valid state.memory handle exclusive known)
    (fun identity => Memory.end_loans_valid state.memory identity known)
    (fun handle => (Memory.release_empty_loan_property Memory.LoanInvariant state.memory handle Value.trivialRelease).mpr known)
    accepted

theorem Continues.resource_loans_valid (program : Array Instruction) (cost pc exit : Nat)
    (s next : StateOf Value) (execution : Continues program cost pc s exit next)
    (known : Memory.LoansValid s.memory) : Memory.LoansValid next.memory := by
  have preserved := run_resource_loans_valid program cost pc s known
  rw [continues_exact_prefix program cost pc exit s next execution] at preserved
  exact preserved

theorem host_run_resource_loans_valid (limits : Memory.Limits) (roots : List Authority.Token)
    (registers : Nat) (s : StateOf Value) (accepted : initialOf Value limits roots registers = .ok s)
    (program : Array Instruction) (fuel pc : Nat) : Memory.LoansValid (run program fuel pc s).1.memory := by
  apply run_resource_loans_valid program fuel pc s
  unfold initialOf at accepted
  split at accepted
  · cases accepted
    simp [Memory.LoansValid, Memory.LoanProperty, Memory.LoanInvariant, Memory.initialOf]
  · cases accepted

end Witchy.Core

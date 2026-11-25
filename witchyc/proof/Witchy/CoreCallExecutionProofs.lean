import Witchy.CoreCallExecution
import Witchy.CoreCallControlProofs
import Witchy.CoreCallLayoutProofs

namespace Witchy.Core.Call

private theorem enter_valid (program : Array Procedure) (machine next : Machine)
    (target : Nat) (arguments destinations : List Nat)
    (known : StorageStateInvariant machine.store) (valid : Memory.LoansValid machine.store.memory)
    (accepted : enter program machine target arguments destinations = .ok next) :
    StorageStateInvariant next.store ∧ Memory.LoansValid next.store.memory := by
  obtain ⟨invariant, _, memory, _⟩ := enter_preserves program machine next target arguments destinations accepted known
  exact ⟨invariant, memory ▸ valid⟩

private theorem leave_valid (program : Array Procedure) (machine next : Machine)
    (known : StorageStateInvariant machine.store) (valid : Memory.LoansValid machine.store.memory)
    (accepted : leave program machine = .ok next) :
    StorageStateInvariant next.store ∧ Memory.LoansValid next.store.memory := by
  obtain ⟨invariant, _, memory, _⟩ := leave_preserves program machine next accepted known
  exact ⟨invariant, memory ▸ valid⟩

private theorem leave_layout_valid (program : Array Procedure) (machine next : Machine) (layout : ReturnLayout)
    (known : StorageStateInvariant machine.store) (valid : Memory.LoansValid machine.store.memory)
    (accepted : leaveLayout program machine layout = .ok next) :
    StorageStateInvariant next.store ∧ Memory.LoansValid next.store.memory := by
  obtain ⟨invariant, _, memory, _⟩ := leave_layout_preserves program machine next layout accepted known
  exact ⟨invariant, memory ▸ valid⟩

theorem advance_preserves (program : Array Procedure) (machine next : Machine)
    (accepted : advance program machine = .ok (some next))
    (known : StorageStateInvariant machine.store) (valid : Memory.LoansValid machine.store.memory) :
    StorageStateInvariant next.store ∧ Memory.LoansValid next.store.memory := by
  simp only [advance, Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals first
    | exact ⟨known, valid⟩
    | exact enter_valid program machine _ _ _ _ known valid (by assumption)
    | exact leave_valid program machine _ known valid (by assumption)
    | exact leave_layout_valid program machine _ _ known valid (by assumption)
    | exact ⟨step_storage_invariant machine.store _ _ known valid (by assumption),
        storage_step_loans machine.store _ _ valid (by assumption)⟩

theorem run_preserves (program : Array Procedure) (fuel : Nat) (machine : Machine)
    (known : StorageStateInvariant machine.store) (valid : Memory.LoansValid machine.store.memory) :
    StorageStateInvariant (run program fuel machine).1.store ∧
      Memory.LoansValid (run program fuel machine).1.store.memory := by
  induction fuel generalizing machine with
  | zero => exact ⟨known, valid⟩
  | succ fuel ih =>
    cases performed : advance program machine with
    | error reason => simpa only [run, performed] using And.intro known valid
    | ok result =>
      cases result with
      | none => simpa only [run, performed] using And.intro known valid
      | some next =>
        obtain ⟨nextKnown, nextValid⟩ := advance_preserves program machine next performed known valid
        simpa only [run, performed] using ih next nextKnown nextValid

end Witchy.Core.Call

import Witchy.LogicObservation
import Witchy.CoreStorageExecution

namespace Witchy.Logic

/-- The common admissible-state invariant is the existing checked ownership
    model, including inventory, well-foundedness, capability validity and loans.
    Bodies do not supply it as a function-specific precondition. -/
structure ValidStore (store : Core.StateOf Core.Value) : Prop where
  storage : Core.StorageStateInvariant store
  loans : Memory.LoansValid store.memory

theorem ValidStore.step (store next : Core.StateOf Core.Value) (instruction : Core.Instruction)
    (valid : ValidStore store) (accepted : Core.step store instruction = .ok next) : ValidStore next :=
  ⟨Core.step_storage_invariant store next instruction valid.storage valid.loans accepted,
    Core.storage_step_loans store next instruction valid.loans accepted⟩

theorem ValidStore.initial (limits : Memory.Limits) (roots : List Authority.Token) (registers : Nat)
    (store : Core.StateOf Core.Value) (host : Core.initialOf Core.Value limits roots registers = .ok store) :
    ValidStore store :=
  ⟨Core.initial_storage_invariant limits roots registers store host,
    Core.host_run_resource_loans_valid limits roots registers store host #[] 0 0⟩

def StoreRealizes (store : Core.StateOf Core.Value) (resource : Resource) : Prop :=
  ValidStore store ∧ resource = observe store

theorem StoreRealizes.agrees (store : Core.StateOf Core.Value) (resource : Resource)
    (known : StoreRealizes store resource) : Agrees store resource := by
  intro location value entry
  rw [known.2] at entry
  exact entry

/-- Regions are interpreted over the actual call machine. Their control-flow
    judgment must additionally bind the procedure, entry and selected exit. -/
def Realizes (machine : Core.Call.Machine) (resource : Resource) : Prop :=
  machine.windowValid ∧ StoreRealizes machine.store resource

end Witchy.Logic

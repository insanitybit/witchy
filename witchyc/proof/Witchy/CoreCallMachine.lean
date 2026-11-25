import Witchy.CoreCallActivation

namespace Witchy.Core.Call

structure Machine where
  store : StateOf Value
  active : Activation
  callers : List Continuation
  deriving Repr, DecidableEq

def Machine.windowValid (machine : Machine) : Prop :=
  machine.active.base + machine.active.width = machine.store.registers.length

instance (machine : Machine) : Decidable machine.windowValid :=
  inferInstanceAs (Decidable (_ = _))

def start (program : Array Procedure) (entry : Nat) (store : StateOf Value) : Result Machine :=
  match program[entry]? with
  | none => .error .instruction
  | some procedure =>
    if procedure.valid ∧ procedure.parameters = [] ∧ procedure.results = [] ∧
        procedure.width = store.registers.length then
      .ok ⟨store, ⟨entry, 0, 0, procedure.width⟩, []⟩
    else .error .instruction

end Witchy.Core.Call

import Witchy.CoreCallMachine
import Witchy.CoreCallBoundary

namespace Witchy.Core.Call

/-- Auxiliary activation windows have arbitrary finite cumulative size. Each
    procedure remains locally bounded; emitted Core has its own physical bounds.
    These checks do not assume successful resource transfers. -/
def EnterPolicy (machine : Machine) (procedure : Procedure) (target : Nat)
    (arguments destinations : List Nat) : Prop :=
  machine.windowValid ∧ procedure.valid ∧
    slotsWithin machine.active.width arguments ∧ slotsWithin machine.active.width destinations ∧
    arguments.Nodup ∧ destinations.Nodup ∧ arguments.length = procedure.parameters.length ∧
    destinations.length = procedure.results.length ∧ target ≠ machine.active.procedure ∧
    (∀ frame ∈ machine.callers, target ≠ frame.caller.procedure)

instance (machine : Machine) (procedure : Procedure) (target : Nat) (arguments destinations : List Nat) :
    Decidable (EnterPolicy machine procedure target arguments destinations) :=
  inferInstanceAs (Decidable (_ ∧ _ ∧ _ ∧ _ ∧ _ ∧ _ ∧ _ ∧ _ ∧ _ ∧ _))

def ReturnPolicy (machine : Machine) (procedure : Procedure) (frame : Continuation) : Prop :=
  machine.windowValid ∧ procedure.valid ∧ procedure.width = machine.active.width ∧
    frame.caller.base + frame.caller.width = machine.active.base ∧
    slotsWithin frame.caller.width frame.destinations ∧ frame.destinations.Nodup ∧
    frame.destinations.length = procedure.results.length

instance (machine : Machine) (procedure : Procedure) (frame : Continuation) :
    Decidable (ReturnPolicy machine procedure frame) :=
  inferInstanceAs (Decidable (_ ∧ _ ∧ _ ∧ _ ∧ _ ∧ _ ∧ _))

def enter (program : Array Procedure) (machine : Machine) (target : Nat)
    (arguments destinations : List Nat) : Result Machine := do
  let procedure ← match program[target]? with
    | some procedure => .ok procedure
    | none => .error .instruction
  if EnterPolicy machine procedure target arguments destinations then
    let base := machine.store.registers.length
    let transfers := (procedure.parameters.map (base + ·)).zip (arguments.map (machine.active.base + ·))
    let store ← enterStore machine.store procedure.width transfers
    let caller := { machine.active with pc := machine.active.pc + 1 }
    return ⟨store, ⟨target, 0, base, procedure.width⟩, ⟨caller, destinations⟩ :: machine.callers⟩
  else .error .instruction

def leave (program : Array Procedure) (machine : Machine) : Result Machine := do
  let procedure ← match program[machine.active.procedure]? with
    | some procedure => .ok procedure
    | none => .error .instruction
  let frame ← match machine.callers with
    | frame :: _ => .ok frame
    | [] => .error .instruction
  if ReturnPolicy machine procedure frame then
    let transfers := (frame.destinations.map (frame.caller.base + ·)).zip
      (procedure.results.map (machine.active.base + ·))
    let store ← leaveStore machine.store machine.active.base transfers
    return ⟨store, frame.caller, machine.callers.tail⟩
  else .error .instruction

end Witchy.Core.Call

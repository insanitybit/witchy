import Witchy.CoreCallControl
import Witchy.CoreCallLayoutControl

namespace Witchy.Core.Call

/-- A fault publishes no intermediate argument or result moves. Fuel-bounded
    execution returns the last complete machine, retaining every live resource. -/
def advance (program : Array Procedure) (machine : Machine)
    (primitive : Nat → Nat → StateOf Value → Core.Instruction → Result (StateOf Value) := fun _ _ => Core.step) : Result (Option Machine) := do
  let procedure ← match program[machine.active.procedure]? with
    | some procedure => .ok procedure
    | none => .error .instruction
  if machine.windowValid ∧ procedure.valid ∧ procedure.width = machine.active.width then
    let instruction ← match procedure.code[machine.active.pc]? with
      | some instruction => .ok instruction
      | none => .error .instruction
    match instruction with
    | .call target arguments destinations => return some (← enter program machine target arguments destinations)
    | .ret => return some (← leave program machine)
    | .retLayout layout => return some (← leaveLayout program machine layout)
    | .core instruction =>
      let instruction ← match relocate machine.active.width machine.active.base instruction with
        | some instruction => .ok instruction
        | none => .error .register
      match instruction with
      | .halt =>
        if machine.callers.isEmpty && Core.terminal machine.store then return none
        else .error .liveOwner
      | .jump target => return some { machine with active.pc := target }
      | .branchFalse condition target =>
        match ← Core.get machine.store condition with
        | .boolean value => return some { machine with active.pc := if value then machine.active.pc + 1 else target }
        | _ => .error .type
      | instruction =>
        let store ← primitive machine.active.procedure machine.active.pc machine.store instruction
        return some { machine with store, active.pc := machine.active.pc + 1 }
  else .error .instruction

def run (program : Array Procedure) : Nat → Machine → Machine × Result Unit
  | 0, machine => (machine, .error .fuel)
  | fuel + 1, machine =>
    match advance program machine with
    | .error reason => (machine, .error reason)
    | .ok none => (machine, .ok ())
    | .ok (some next) => run program fuel next

end Witchy.Core.Call

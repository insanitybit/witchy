import Witchy.CoreCallControl

namespace Witchy.Core.Call

/-- Outcome-dependent return. The layout must cover the entire declared result
    signature exactly, including inactive fields. All caller destinations must
    be vacant. Failure publishes neither moves nor a new control frame. -/
def leaveLayout (program : Array Procedure) (machine : Machine) (layout : ReturnLayout) : Result Machine := do
  let procedure ← match program[machine.active.procedure]? with
    | some procedure => .ok procedure
    | none => .error .instruction
  let frame ← match machine.callers with
    | frame :: _ => .ok frame
    | [] => .error .instruction
  if ReturnPolicy machine procedure frame ∧ layout.slots = procedure.results then
    if vacantSlots machine.store frame.caller.base frame.destinations then
      let selected ← layout.select machine.store machine.active.base
      let transfers := layoutTransfers frame.caller.base machine.active.base frame.destinations procedure.results selected
      let store ← leaveStore machine.store machine.active.base transfers
      return ⟨store, frame.caller, machine.callers.tail⟩
    else .error .occupied
  else .error .instruction

end Witchy.Core.Call

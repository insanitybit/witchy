import Witchy.CoreStep

namespace Witchy.Core.Call

/-- Activations use disjoint windows of the existing register inventory.
    No values are held in control frames outside that inventory. -/
def openRegisters (s : StateOf Value) (count : Nat) : StateOf Value :=
  { s with registers := s.registers ++ List.replicate count none }

/-- A return may discard only vacant and scalar scratch cells. A capability
    must be returned or otherwise explicitly accounted for, just like an owner. -/
def returnScratch : Option Value → Bool
  | none | some (.number _) | some (.boolean _) | some (.scalarAggregate _) => true
  | _ => false

def closeRegisters (s : StateOf Value) (base : Nat) : Result (StateOf Value) :=
  if base ≤ s.registers.length ∧ (s.registers.drop base).all returnScratch then
    .ok { s with registers := s.registers.take base }
  else .error .liveOwner

/-- Parameter/result transfer delegates to the existing consuming instruction.
    Publishing the new control state happens only after the entire list succeeds. -/
def moveRegisters : StateOf Value → List (Nat × Nat) → Result (StateOf Value)
  | s, [] => .ok s
  | s, (destination, source) :: rest => do
    let next ← Core.step s (.move destination source)
    moveRegisters next rest

end Witchy.Core.Call

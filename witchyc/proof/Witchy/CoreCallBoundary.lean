import Witchy.CoreCallRegisters

namespace Witchy.Core.Call

def enterStore (store : StateOf Value) (width : Nat) (transfers : List (Nat × Nat)) : Result (StateOf Value) :=
  moveRegisters (openRegisters store width) transfers

def leaveStore (store : StateOf Value) (base : Nat) (transfers : List (Nat × Nat)) : Result (StateOf Value) := do
  let moved ← moveRegisters store transfers
  closeRegisters moved base

end Witchy.Core.Call

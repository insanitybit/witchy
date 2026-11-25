import Witchy.CoreCallRegisters

namespace Witchy.Core.Call

/-- A procedure-local result representation. Products compose records; choices
    use the same Boolean tag and disjoint payload slots as source sums. -/
inductive ReturnLayout where
  | fields (slots : List Nat)
  | product (left right : ReturnLayout)
  | choice (tag : Nat) (yes no : ReturnLayout)
  deriving Repr, DecidableEq

def ReturnLayout.slots : ReturnLayout → List Nat
  | .fields slots => slots
  | .product left right => left.slots ++ right.slots
  | .choice tag yes no => tag :: (yes.slots ++ no.slots)

/-- Out-of-window cells are not vacant cells. Inactive payloads must actually
    be vacant, even if they would otherwise be discardable scalar scratch. -/
def vacantSlots (store : StateOf Value) (base : Nat) (slots : List Nat) : Bool :=
  slots.all fun slot => decide (store.registers[base + slot]? = some none)

/-- Resolve only active fields. This does not move values or grant a contract
    premise: ordinary consuming moves still check every selected field. -/
def ReturnLayout.select (store : StateOf Value) (base : Nat) : ReturnLayout → Result (List Nat)
  | .fields slots => .ok slots
  | .product left right => do
    let first ← left.select store base
    let second ← right.select store base
    return first ++ second
  | .choice tag yes no => do
    match ← Core.get store (base + tag) with
    | .boolean true =>
      if vacantSlots store base no.slots then
        return tag :: (← yes.select store base)
      else .error .liveOwner
    | .boolean false =>
      if vacantSlots store base yes.slots then
        return tag :: (← no.select store base)
      else .error .liveOwner
    | _ => .error .type

/-- Keep the full destination ABI, selecting corresponding source/destination
    pairs together. Inactive destinations are not packed or renumbered. -/
def layoutTransfers (callerBase calleeBase : Nat) (destinations results selected : List Nat) : List (Nat × Nat) :=
  ((destinations.zip results).filter fun pair => pair.2 ∈ selected).map
    fun pair => (callerBase + pair.1, calleeBase + pair.2)

end Witchy.Core.Call

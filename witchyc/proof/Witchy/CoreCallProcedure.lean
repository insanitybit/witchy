import Witchy.CoreCallInstruction

namespace Witchy.Core.Call

structure Procedure where
  width : Nat
  parameters : List Nat
  results : List Nat
  code : Array Instruction
  deriving Repr, DecidableEq

def slotsWithin (width : Nat) (slots : List Nat) : Prop :=
  ∀ index ∈ slots, index < width

instance (width : Nat) (slots : List Nat) : Decidable (slotsWithin width slots) :=
  inferInstanceAs (Decidable (∀ index ∈ slots, index < width))

def Procedure.valid (procedure : Procedure) : Prop :=
  procedure.width ≤ 4096 ∧ slotsWithin procedure.width procedure.parameters ∧
    slotsWithin procedure.width procedure.results ∧ procedure.parameters.Nodup ∧ procedure.results.Nodup

instance (procedure : Procedure) : Decidable procedure.valid :=
  inferInstanceAs (Decidable (_ ∧ _ ∧ _ ∧ _ ∧ _))

end Witchy.Core.Call

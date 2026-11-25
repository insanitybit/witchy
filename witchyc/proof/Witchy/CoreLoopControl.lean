import Witchy.CoreBooleanEffects
import Witchy.CoreComposition

namespace Witchy.Core

variable {Cell : Type} [StorageCell Cell]

/-- Exact comparison/branch behavior for both entering and exiting a loop. -/
theorem run_less_branch (program : Array Instruction) (fuel pc exit : Nat) (s : StateOf Cell)
    (condition left right : Nat) (a b : UInt64) (slot : BoolSlot s condition)
    (leftValue : number s left = .ok a) (rightValue : number s right = .ok b)
    (first : program[pc]? = some (.calculate condition left right .less))
    (second : program[pc + 1]? = some (.branchFalse condition exit)) :
    run program (fuel + 2) pc s =
      run program fuel (if a < b then pc + 2 else exit)
        (withBool s condition (decide (a < b))) := by
  have comparison := less_effect s condition left right a b slot leftValue rightValue
  simp only [run, first, comparison, second, with_bool_get s condition _ slot, Nat.add_assoc]
  simp

theorem run_back_edge (program : Array Instruction) (fuel pc header : Nat) (s : StateOf Cell)
    (instruction : program[pc]? = some (.jump header)) :
    run program (fuel + 1) pc s = run program fuel header s := by
  simp only [run, instruction]

theorem less_branch_continues (program : Array Instruction) (pc exit : Nat) (s : StateOf Cell)
    (condition left right : Nat) (a b : UInt64) (slot : BoolSlot s condition)
    (leftValue : number s left = .ok a) (rightValue : number s right = .ok b)
    (first : program[pc]? = some (.calculate condition left right .less))
    (second : program[pc + 1]? = some (.branchFalse condition exit)) :
    Continues program 2 pc s (if a < b then pc + 2 else exit)
      (withBool s condition (decide (a < b))) := by
  intro fuel
  exact run_less_branch program fuel pc exit s condition left right a b slot leftValue rightValue first second

theorem back_edge_continues (program : Array Instruction) (pc header : Nat) (s : StateOf Cell)
    (instruction : program[pc]? = some (.jump header)) : Continues program 1 pc s header s := by
  intro fuel
  exact run_back_edge program fuel pc header s instruction

end Witchy.Core

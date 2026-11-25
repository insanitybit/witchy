import Witchy.CoreCallExecution

namespace Witchy.Core.Call

/-- Finite prefixes of the actual call executor, rather than a second function
    semantics. The witness may include calls, returns and ordinary instructions. -/
inductive Path (program : Array Procedure) : Nat → Machine → Machine → Prop where
  | refl (machine : Machine) : Path program 0 machine machine
  | advance {cost : Nat} {machine next final : Machine}
      (edge : advance program machine = .ok (some next))
      (rest : Path program cost next final) : Path program (cost + 1) machine final

theorem Path.continues (program : Array Procedure) (cost : Nat) (machine final : Machine)
    (path : Path program cost machine final) (fuel : Nat) :
    run program (cost + fuel) machine = run program fuel final := by
  induction path with
  | refl machine => simp only [Nat.zero_add]
  | @advance cost machine next final edge rest ih =>
    rw [show cost + 1 + fuel = (cost + fuel) + 1 by omega]
    simp only [run, edge]
    exact ih

theorem Path.append {program : Array Procedure} {firstCost secondCost : Nat} {start middle final : Machine}
    (first : Path program firstCost start middle) (second : Path program secondCost middle final) :
    Path program (firstCost + secondCost) start final := by
  induction first with
  | refl => simpa only [Nat.zero_add] using second
  | advance edge rest ih =>
    have combined := Path.advance edge (ih second)
    simpa only [Nat.add_right_comm] using combined

end Witchy.Core.Call

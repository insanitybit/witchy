import Witchy.RecursiveCleanup

namespace Witchy.Storage

/-- Once cleanup completes, one extra fuel unit cannot change its result.
    This permits administrative Core steps without changing traversal semantics. -/
theorem run_cleanup_complete_succ_fuel (fuel : Nat) (memory : Memory.StateOf Core.Value)
    (pending : List Core.Value) (complete : (runCleanup fuel memory pending).2.1 = .complete) :
    runCleanup (fuel + 1) memory pending = runCleanup fuel memory pending := by
  induction fuel generalizing memory pending with
  | zero =>
    cases pending with
    | nil => rfl
    | cons value tail => simp [runCleanup] at complete
  | succ fuel ih =>
    cases pending with
    | nil => rfl
    | cons value tail =>
      cases performed : cleanupWorkStep memory (value :: tail) with
      | mk next pair =>
        cases pair with
        | mk outcome rest =>
          cases outcome with
          | error reason => simp [runCleanup, performed] at complete
          | ok doneValue =>
            cases doneValue
            have restComplete : (runCleanup fuel next rest).2.1 = .complete := by
              simpa only [runCleanup, performed] using complete
            simpa only [runCleanup, performed] using ih next rest restComplete

/-- Completion is stable under arbitrary additional fuel, not just a particular
    syntactic instruction count. No assumption about loans or ownership is needed. -/
theorem run_cleanup_complete_more_fuel (fuel extra : Nat) (memory : Memory.StateOf Core.Value)
    (pending : List Core.Value) (complete : (runCleanup fuel memory pending).2.1 = .complete) :
    runCleanup (fuel + extra) memory pending = runCleanup fuel memory pending := by
  induction extra with
  | zero => rfl
  | succ extra ih =>
    have nextComplete : (runCleanup (fuel + extra) memory pending).2.1 = .complete := by rw [ih]; exact complete
    exact (run_cleanup_complete_succ_fuel (fuel + extra) memory pending nextComplete).trans ih

end Witchy.Storage

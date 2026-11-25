import Witchy.LogicScalar
import Witchy.LogicServiceFacts

namespace Witchy.Logic

/-- Read-only access to the actual loan-service budget. Zero requests succeed
    exactly when the existing live table fits; arithmetic uses unbounded Nat.
    This grants no loan and changes no memory, counter, authority, or frame. -/
theorem loan_room_rule (dst count : Nat) (requested : UInt64) (counter live limit : Nat) :
    AtomicRule (.loanRoom dst count)
      (fun resource => ScalarSlot resource dst (.boolean false) ∧
        resource (.register count) = some (some (.number requested)) ∧
        resource .loanSupply = some (counter, live, limit))
      (ScalarOutput dst (.boolean (decide (live + requested.toNat ≤ limit)))) := by
  apply scalar_rule _ dst _ _ (fun _ input => input.1)
  intro resource store input known
  have actual := known (.register count) _ input.2.1
  change store.registers[count]? = some (some (.number requested)) at actual
  have supply := loan_supply_observed store resource counter live limit known input.2.2
  simp only [Core.step, Core.number, Core.get, actual, Option.bind_some, id, Bind.bind, Pure.pure,
    supply.2.1, supply.2.2]

/-- Capacity for both migration borrows is tested before allocation or mutation;
    the success branch can split the checked credit without a function precondition. -/
theorem two_loans_fit (live limit : Nat) (guard : live + 2 ≤ limit) :
    live < limit ∧ live + 1 < limit := by omega

theorem one_loan_refused (live limit : Nat) (guard : ¬ live + 1 ≤ limit) :
    ¬ live < limit := by omega

end Witchy.Logic

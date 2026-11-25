import Witchy.LogicServiceFacts
import Witchy.LogicEnd

namespace Witchy.Logic

structure EndInput (resource : Resource) (source identity counter live limit : Nat)
    (loan : Memory.Loan) (permissions : List Memory.Loan) : Prop where
  reference : resource (.register source) = some (some (.reference identity))
  entry : resource (.loan identity) = some loan
  permissionsEntry : resource (.permissions loan.target) = some permissions
  service : resource .loanSupply = some (counter, live, limit)

def EndOutput (before : Resource) (source identity counter live limit : Nat)
    (loan : Memory.Loan) (permissions : List Memory.Loan) (result : Core.Result Unit) (after : Resource) : Prop :=
  result = .ok () ∧ after (.register source) = some none ∧ after (.loan identity) = none ∧
    after (.loanSupply) = some (counter, live - 1, limit) ∧
    after (.permissions loan.target) = some (permissions.filter (fun current => current.identity != identity)) ∧
    Resource.SameOutside [.register source, .loan identity, .permissions loan.target, .loanSupply] before after

theorem end_rule (source identity counter live limit : Nat) (loan : Memory.Loan)
    (permissions : List Memory.Loan) :
    AtomicRule (.endLoan source)
      (fun resource => EndInput resource source identity counter live limit loan permissions)
      (fun before => EndOutput before source identity counter live limit loan permissions) := by
  apply AtomicRule.of_effect
  intro resource store input valid known
  have reference := known (.register source) _ input.reference
  have found := known (.loan identity) loan input.entry
  change Memory.loan? store.memory identity = some loan at found
  have oldPermissions := known (.permissions loan.target) permissions input.permissionsEntry
  have supply := loan_supply_observed store resource counter live limit known input.service
  obtain ⟨next, performed, effect, cleared, retired, memory⟩ :=
    end_locality store source identity loan reference valid.loans found
  rw [performed]
  refine ⟨[.register source, .loan identity, .permissions loan.target, .loanSupply], ?_, effect, ?_⟩
  · intro location member
    simp only [List.mem_cons, List.not_mem_nil, or_false] at member
    rcases member with rfl | rfl | rfl | rfl
    · exact .inl (Resource.occupied input.reference)
    · exact .inl (Resource.occupied input.entry)
    · exact .inl (Resource.occupied input.permissionsEntry)
    · exact .inl (Resource.occupied input.service)
  · refine ⟨rfl, ?_, ?_, ?_, ?_, Resource.patch_same_outside _ _ _⟩
    · exact (Resource.patch_at _ _ _ _ (by simp)).trans cleared
    · exact (Resource.patch_at _ _ _ _ (by simp)).trans retired
    · apply (Resource.patch_at _ _ _ _ (by simp)).trans
      change some (next.memory.nextLoan, next.memory.loans.length, next.memory.limits.loans) = _
      rw [memory]
      have length := Memory.filter_unique_identity_length store.memory.loans identity loan valid.loans.2.2.1 found
      have count : (Memory.endLoan store.memory identity).1.loans.length = live - 1 := by
        simp only [Memory.endLoan, found]
        have beforeCount := supply.2.1
        omega
      rw [count]
      simp only [Memory.endLoan, found]
      rw [supply.1, supply.2.2]
    · apply (Resource.patch_at _ _ _ _ (by simp)).trans
      change (Memory.block? next.memory loan.target).map (fun _ => Memory.active next.memory loan.target) = _
      rw [memory]
      have blocks : Memory.block? (Memory.endLoan store.memory identity).1 loan.target =
          Memory.block? store.memory loan.target := by simp [Memory.block?, Memory.endLoan, found] <;> rfl
      rw [blocks, Memory.active_after_end]
      change (Memory.block? store.memory loan.target).map (fun _ => Memory.active store.memory loan.target) = some permissions at oldPermissions
      have transformed := congrArg (fun view => view.map (fun loans : List Memory.Loan =>
        loans.filter (fun current => current.identity != identity))) oldPermissions
      simpa only [Option.map_map, Function.comp_def, Option.map_some] using transformed

end Witchy.Logic

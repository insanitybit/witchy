import Witchy.LogicServiceFacts
import Witchy.LogicBorrow

namespace Witchy.Logic

structure BorrowInput (resource : Resource) (dst src handle counter live limit : Nat)
    (exclusive : Bool) (block : Storage.Block Core.Value) (permissions : List Memory.Loan) : Prop where
  destination : resource (.register dst) = some none
  owner : resource (.register src) = some (some (.owner handle))
  storage : resource (.block handle) = some block
  permissionsEntry : resource (.permissions handle) = some permissions
  rights : (if exclusive then permissions.isEmpty else permissions.all (fun loan => !loan.exclusive)) = true
  service : resource .loanSupply = some (counter, live, limit)

def BorrowOutput (before : Resource) (dst handle counter live limit : Nat) (exclusive : Bool)
    (permissions : List Memory.Loan) (result : Core.Result Unit) (after : Resource) : Prop :=
  if live < limit then
    result = .ok () ∧ after (.register dst) = some (some (.reference counter)) ∧
      after (.loan counter) = some ⟨counter, handle, exclusive⟩ ∧
      after (.permissions handle) = some (⟨counter, handle, exclusive⟩ :: permissions) ∧
      after .loanSupply = some (counter + 1, live + 1, limit) ∧
      Resource.SameOutside [.register dst, .loan counter, .permissions handle, .loanSupply] before after
  else result = .error (.memory .metadata) ∧ after = before

theorem borrow_rule (dst src handle counter live limit : Nat) (exclusive : Bool)
    (block : Storage.Block Core.Value) (permissions : List Memory.Loan) :
    AtomicRule (.borrow dst src exclusive)
      (fun resource => BorrowInput resource dst src handle counter live limit exclusive block permissions)
      (fun before => BorrowOutput before dst handle counter live limit exclusive permissions) := by
  apply AtomicRule.of_effect
  intro resource store input valid known
  have empty := known (.register dst) none input.destination
  have owner := known (.register src) _ input.owner
  have found := known (.block handle) block input.storage
  change Memory.block? store.memory handle = some block at found
  have oldPermissions := permissions_observed store handle permissions
    (known (.permissions handle) permissions input.permissionsEntry)
  have rights : Memory.borrowable store.memory handle exclusive = true := by
    simpa only [Memory.borrowable, oldPermissions] using input.rights
  have supply := loan_supply_observed store resource counter live limit known input.service
  by_cases room : live < limit
  · have actualRoom : store.memory.loans.length < store.memory.limits.loans := by
      rw [supply.2.1, supply.2.2]
      exact room
    obtain ⟨next, performed, effect, output, memory⟩ :=
      borrow_success_locality store dst src handle exclusive block empty owner found rights actualRoom
    rw [supply.1] at effect output
    rw [performed]
    refine ⟨[.register dst, .loan counter, .permissions handle, .loanSupply], ?_, effect, ?_⟩
    · intro location member
      simp only [List.mem_cons, List.not_mem_nil, or_false] at member
      rcases member with rfl | rfl | rfl | rfl
      · exact .inl (Resource.occupied input.destination)
      · right
        rw [← supply.1]
        exact fresh_loan_absent store valid
      · exact .inl (Resource.occupied input.permissionsEntry)
      · exact .inl (Resource.occupied input.service)
    · unfold BorrowOutput
      rw [ite_eq_left room]
      refine ⟨rfl, ?_, ?_, ?_, ?_, Resource.patch_same_outside _ _ _⟩
      · exact (Resource.patch_at _ _ _ _ (by simp)).trans output
      · apply (Resource.patch_at _ _ _ _ (by simp)).trans
        change Memory.loan? next.memory counter = some ⟨counter, handle, exclusive⟩
        rw [memory]
        simp [borrowedMemory, Memory.loan?, supply.1]
      · apply (Resource.patch_at _ _ _ _ (by simp)).trans
        change (Memory.block? next.memory handle).map (fun _ => Memory.active next.memory handle) = _
        rw [memory]
        change (Memory.block? store.memory handle).map
          (fun _ => Memory.active (borrowedMemory store.memory handle exclusive) handle) = _
        rw [found, borrowed_permissions, oldPermissions, supply.1]
        rfl
      · apply (Resource.patch_at _ _ _ _ (by simp)).trans
        change some (next.memory.nextLoan, next.memory.loans.length, next.memory.limits.loans) = _
        rw [memory]
        simp only [borrowedMemory, List.length_cons]
        rw [supply.1, supply.2.1, supply.2.2]
  · have actualFull : ¬ store.memory.loans.length < store.memory.limits.loans := by
      rw [supply.2.1, supply.2.2]
      exact room
    rw [borrow_exhaustion store dst src handle exclusive block empty owner found actualFull]
    change BorrowOutput resource dst handle counter live limit exclusive permissions (.error (.memory .metadata)) resource
    unfold BorrowOutput
    rw [ite_eq_right room]
    exact ⟨rfl, rfl⟩

end Witchy.Logic

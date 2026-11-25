import Witchy.Memory

namespace Witchy.Memory

theorem initial_quota (limits : Limits) : quota (initial limits) = limits.slots := by
  simp [quota, initial, capacity]

theorem allocation_failure_atomic (s : StateOf Value) (count : Nat) (e : Error)
    (he : (allocate s count).2 = .error e) : (allocate s count).1 = s := by
  by_cases hm : s.blocks.length < s.limits.allocations
  · by_cases ha : count ≤ s.available
    · simp [allocate, hm, ha] at he
    · simp [allocate, hm, ha]
  · simp [allocate, hm]

theorem allocation_preserves_quota (s : StateOf Value) (count : Nat) :
    quota (allocate s count).1 = quota s := by
  by_cases hm : s.blocks.length < s.limits.allocations
  · by_cases ha : count ≤ s.available
    · simp only [allocate, hm, ha, ↓reduceIte, quota, capacity, List.map_append,
        List.map_cons, List.map_nil, List.sum_append, List.sum_cons, List.sum_nil,
        Option.map_some, Option.getD_some, List.length_replicate, Storage.Block.slots_mk]
      omega
    · simp [allocate, hm, ha]
  · simp [allocate, hm]

theorem allocation_fresh (s : StateOf Value) (count : Nat) (h : Handle)
    (he : (allocate s count).2 = .ok h) :
    h = s.blocks.length ∧ block? (allocate s count).1 h = some ⟨List.replicate count none⟩ := by
  by_cases hm : s.blocks.length < s.limits.allocations
  · by_cases ha : count ≤ s.available
    · simp only [allocate, hm, ha, ↓reduceIte, Result.ok.injEq] at he
      subst h
      simp [allocate, hm, ha, block?]
    · simp [allocate, hm, ha] at he
  · simp [allocate, hm] at he

/-- Every successful read is from a live allocation and an initialized, in-range
    slot, after an accepted permission resolution. No assertion grants permission. -/
theorem read_safe (s : State) (access : Access) (index : Nat) (value : UInt64)
    (hr : read s access index = .ok value) :
    ∃ h b, resolve s access false = .ok h ∧ block? s h = some b ∧
      index < b.slots.length ∧ b.slots[index]? = some (some value) := by
  unfold read at hr
  cases hp : resolve s access false with
  | error e => simp [hp] at hr
  | ok h =>
    cases hb : block? s h with
    | none => simp [hp, hb] at hr
    | some b =>
      cases hs : b.slots[index]? with
      | none => simp [hp, hb, hs] at hr
      | some cell =>
        cases cell with
        | none => simp [hp, hb, hs] at hr
        | some v =>
          have heq : v = value := by simpa [hp, hb, hs] using hr
          subst v
          exact ⟨h, b, rfl, hb, List.getElem?_eq_some_iff.mp hs |>.1, hs⟩

theorem owner_access_blocked (s : State) (h : Handle)
    (hl : (active s h).isEmpty = false) (write : Bool) :
    resolve s (.owner h) write = .error .borrowed := by
  simp [resolve, hl]

theorem read_loan_cannot_write (s : StateOf Value) (id : LoanId) (l : Loan)
    (hl : loan? s id = some l) (hr : l.exclusive = false) :
    resolve s (.loan id) true = .error .permission := by
  simp [resolve, hl, hr]

theorem ended_loan_is_invalid (s : StateOf Value) (id : LoanId) (l : Loan)
    (hl : loan? s id = some l) : loan? (endLoan s id).1 id = none := by
  simp only [endLoan, hl]
  simp [loan?, List.find?_eq_none]

theorem released_handle_invalid (s : State) (h : Handle) (b : Block)
    (hb : block? s h = some b) (ha : (active s h).isEmpty = true) :
    block? (release s h).1 h = none := by
  have hi : h < s.blocks.length := by
    unfold block? at hb
    cases hh : s.blocks[h]? <;> simp_all
    exact List.getElem?_eq_some_iff.mp hh |>.1
  simp only [release, hb, ha, ↓reduceIte]
  simp [block?, hi]

end Witchy.Memory

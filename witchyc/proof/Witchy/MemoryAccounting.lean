import Witchy.MemoryProofs

namespace Witchy.Memory

def weight (b : Option Block) : Nat := (b.map (fun b => b.slots.length)).getD 0

theorem capacity_set (blocks : List (Option Block)) (index : Nat) (old new : Option Block)
    (h : blocks[index]? = some old) :
    capacity (blocks.set index new) + weight old = capacity blocks + weight new := by
  induction blocks generalizing index with
  | nil => simp at h
  | cons head tail ih =>
    cases index with
    | zero =>
      simp only [List.getElem?_cons_zero, Option.some.injEq] at h
      subst head
      simp [capacity, weight, Nat.add_comm, Nat.add_left_comm]
    | succ i =>
      simp only [List.getElem?_cons_succ] at h
      have ht := ih i h
      simp only [List.set, capacity, List.map_cons, List.sum_cons] at *
      omega

theorem block_lookup (s : StateOf Value) (h : Handle) (b : Storage.Block Value) (hb : block? s h = some b) :
    s.blocks[h]? = some (some b) := by
  exact Option.join_eq_some_iff.mp hb

theorem release_preserves_quota (s : State) (h : Handle) :
    quota (release s h).1 = quota s := by
  cases hb : block? s h with
  | none => simp [release, hb]
  | some b =>
    by_cases ha : (active s h).isEmpty = true
    · have hc := capacity_set s.blocks h (some b) none (block_lookup s h b hb)
      simp only [weight, Option.map_some, Option.getD_some, Option.map_none, Option.getD_none] at hc
      simp only [release, hb, ha, ↓reduceIte, quota]
      omega
    · simp [release, hb, ha]

theorem replace_slots_preserves_quota (s : State) (h : Handle) (old : Block)
    (slots : List (Option UInt64)) (hb : block? s h = some old)
    (length : slots.length = old.slots.length) :
    quota { s with blocks := s.blocks.set h (some ⟨slots⟩) } = quota s := by
  have accounting := capacity_set s.blocks h (some old) (some ⟨slots⟩) (block_lookup s h old hb)
  simp only [weight, Option.map_some, Option.getD_some, length, Storage.Block.slots_mk] at accounting
  have equal : capacity (s.blocks.set h (some ⟨slots⟩)) = capacity s.blocks := by omega
  simp only [quota, equal]

theorem write_preserves_quota (s : State) (access : Access) (i : Nat) (value : UInt64) :
    quota (writeEmpty s access i value).1 = quota s := by
  cases hp : resolve s access true with
  | error reason => simp [writeEmpty, hp]
  | ok h =>
    cases hb : block? s h with
    | none => simp [writeEmpty, hp, hb]
    | some block =>
      cases cell : block.slots[i]? with
      | none => simp [writeEmpty, hp, hb, cell]
      | some slot =>
        cases slot with
        | some old => simp [writeEmpty, hp, hb, cell]
        | none =>
          simpa only [writeEmpty, hp, hb, cell] using
            replace_slots_preserves_quota s h block (block.slots.set i (some value)) hb (by simp)

theorem take_preserves_quota (s : State) (access : Access) (i : Nat) :
    quota (take s access i).1 = quota s := by
  cases hp : resolve s access true with
  | error reason => simp [take, hp]
  | ok h =>
    cases hb : block? s h with
    | none => simp [take, hp, hb]
    | some block =>
      cases cell : block.slots[i]? with
      | none => simp [take, hp, hb, cell]
      | some slot =>
        cases slot with
        | none => simp [take, hp, hb, cell]
        | some value =>
          simpa only [take, hp, hb, cell] using
            replace_slots_preserves_quota s h block (block.slots.set i none) hb (by simp)

theorem borrow_preserves_quota (s : State) (h : Handle) (exclusive : Bool) :
    quota (borrow s h exclusive).1 = quota s := by
  unfold borrow
  repeat' first | rfl | split

theorem end_preserves_quota (s : State) (key : LoanId) : quota (endLoan s key).1 = quota s := by
  unfold endLoan
  split <;> rfl

theorem no_live_blocks_capacity_zero (blocks : List (Option Block))
    (empty : ∀ block ∈ blocks, block = none) : capacity blocks = 0 := by
  induction blocks with
  | nil => rfl
  | cons block rest ih =>
    have first : block = none := empty block (by simp)
    have tail : ∀ block ∈ rest, block = none := fun block member => empty block (by simp [member])
    subst block
    simpa [capacity] using ih tail

theorem write_failure_atomic (s : State) (access : Access) (i : Nat) (v : UInt64) (e : Error)
    (he : (writeEmpty s access i v).2 = .error e) : (writeEmpty s access i v).1 = s := by
  cases hp : resolve s access true with
  | error reason => simp [writeEmpty, hp]
  | ok h =>
    cases hb : block? s h with
    | none => simp [writeEmpty, hp, hb]
    | some b =>
      cases hs : b.slots[i]? with
      | none => simp [writeEmpty, hp, hb, hs]
      | some cell =>
        cases cell with
        | none => simp [writeEmpty, hp, hb, hs] at he
        | some value => simp [writeEmpty, hp, hb, hs]

theorem take_failure_atomic (s : State) (access : Access) (i : Nat) (e : Error)
    (he : (take s access i).2 = .error e) : (take s access i).1 = s := by
  cases hp : resolve s access true with
  | error reason => simp [take, hp]
  | ok h =>
    cases hb : block? s h with
    | none => simp [take, hp, hb]
    | some b =>
      cases hs : b.slots[i]? with
      | none => simp [take, hp, hb, hs]
      | some cell =>
        cases cell with
        | none => simp [take, hp, hb, hs]
        | some value => simp [take, hp, hb, hs] at he

end Witchy.Memory

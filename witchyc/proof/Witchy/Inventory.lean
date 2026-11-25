import Init

namespace Witchy.Storage

/-- Replacing one present entry transfers exactly its old/new inventory weight. -/
theorem sum_set_weight (items : List Value) (index : Nat) (old replacement : Value)
    (weight : Value → Nat) (present : items[index]? = some old) :
    ((items.set index replacement).map weight).sum + weight old =
      (items.map weight).sum + weight replacement := by
  induction items generalizing index with
  | nil => simp at present
  | cons head tail ih =>
    cases index with
    | zero =>
      simp only [List.getElem?_cons_zero, Option.some.injEq] at present
      subst head
      simp [Nat.add_comm, Nat.add_left_comm]
    | succ index =>
      have rest := ih index (by simpa using present)
      simp only [List.set, List.map_cons, List.sum_cons] at rest ⊢
      omega

end Witchy.Storage

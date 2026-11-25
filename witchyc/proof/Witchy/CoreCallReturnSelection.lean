import Witchy.CoreCallReturnLayout

namespace Witchy.Core.Call

/-- Logical interpretation of the selected representation. It exposes the
    actual outcome tag and inactive vacancy, not generated instruction PCs. -/
inductive ReturnLayout.Selected (store : StateOf Value) (base : Nat) : ReturnLayout → List Nat → Prop where
  | fields (slots : List Nat) : Selected store base (.fields slots) slots
  | product {left right : ReturnLayout} {first second : List Nat}
      (leftSelected : Selected store base left first) (rightSelected : Selected store base right second) :
      Selected store base (.product left right) (first ++ second)
  | positive {tag : Nat} {yes no : ReturnLayout} {selected : List Nat}
      (tagged : Core.get store (base + tag) = .ok (.boolean true))
      (inactive : vacantSlots store base no.slots = true) (active : Selected store base yes selected) :
      Selected store base (.choice tag yes no) (tag :: selected)
  | negative {tag : Nat} {yes no : ReturnLayout} {selected : List Nat}
      (tagged : Core.get store (base + tag) = .ok (.boolean false))
      (inactive : vacantSlots store base yes.slots = true) (active : Selected store base no selected) :
      Selected store base (.choice tag yes no) (tag :: selected)

theorem ReturnLayout.select_sound (store : StateOf Value) (base : Nat) (layout : ReturnLayout) (selected : List Nat)
    (accepted : layout.select store base = .ok selected) : layout.Selected store base selected := by
  induction layout generalizing selected with
  | fields slots => cases accepted; exact .fields slots
  | product left right ihLeft ihRight =>
    simp only [select, Bind.bind, Pure.pure] at accepted
    repeat' first | split at accepted | cases accepted
    exact .product (ihLeft _ (by assumption)) (ihRight _ (by assumption))
  | choice tag yes no ihYes ihNo =>
    simp only [select, Bind.bind, Pure.pure] at accepted
    repeat' first | split at accepted | cases accepted
    all_goals first
      | exact .positive (by assumption) (by assumption) (ihYes _ (by assumption))
      | exact .negative (by assumption) (by assumption) (ihNo _ (by assumption))

theorem ReturnLayout.Selected.sublist {store : StateOf Value} {base : Nat} {layout : ReturnLayout} {selected : List Nat}
    (selection : layout.Selected store base selected) : selected.Sublist layout.slots := by
  induction selection with
  | fields slots => exact List.Sublist.refl slots
  | product left right ihLeft ihRight => exact ihLeft.append ihRight
  | positive tagged inactive active ih => exact .cons_cons _ (List.sublist_append_of_sublist_left ih)
  | negative tagged inactive active ih => exact .cons_cons _ (List.sublist_append_of_sublist_right ih)

theorem ReturnLayout.Selected.subset {store : StateOf Value} {base : Nat} {layout : ReturnLayout} {selected : List Nat}
    (selection : layout.Selected store base selected) : ∀ slot ∈ selected, slot ∈ layout.slots := by
  exact fun _ member => selection.sublist.subset member

theorem ReturnLayout.Selected.nodup {store : StateOf Value} {base : Nat} {layout : ReturnLayout} {selected : List Nat}
    (selection : layout.Selected store base selected) (signature : layout.slots.Nodup) : selected.Nodup :=
  selection.sublist.nodup signature

theorem ReturnLayout.Selected.complete {store : StateOf Value} {base : Nat} {layout : ReturnLayout} {selected : List Nat}
    (selection : layout.Selected store base selected) : layout.select store base = .ok selected := by
  induction selection with
  | fields slots => rfl
  | product left right ihLeft ihRight => simp only [select, ihLeft, ihRight, Bind.bind, Pure.pure]
  | positive tagged inactive active ih => simp only [select, tagged, Bind.bind, inactive, ↓reduceIte, ih, Pure.pure]
  | negative tagged inactive active ih => simp only [select, tagged, Bind.bind, inactive, ↓reduceIte, ih, Pure.pure]

theorem vacant_slots_at (store : StateOf Value) (base : Nat) (slots : List Nat)
    (vacant : vacantSlots store base slots = true) (slot : Nat) (member : slot ∈ slots) :
    store.registers[base + slot]? = some none := by
  exact of_decide_eq_true (List.all_eq_true.mp vacant slot member)

end Witchy.Core.Call

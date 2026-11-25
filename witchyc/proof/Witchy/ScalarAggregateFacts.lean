import Witchy.TypedStorageRuntime
namespace Witchy.Storage
/-- These total projections create no facts: every use carries the typed-cell premise. -/
@[simp] def aggregateFields : Option Core.Value → List UInt64
  | some (.scalarAggregate fields) => fields
  | _ => []
@[simp] def aggregateField (cell : Option Core.Value) (index : Nat) : UInt64 := (aggregateFields cell)[index]?.getD 0
@[simp] def projectFields (fields : List UInt64) (count : Nat) : List UInt64 :=
  List.ofFn (fun i : Fin count => fields[i.val]?.getD 0)
theorem projectFields_eq (fields : List UInt64) : projectFields fields fields.length = fields := by
  simp [projectFields]

theorem scalar_fields_length (widths : List Nat) (fields : List UInt64)
    (valid : Core.scalarFieldsValid widths fields = true) : widths.length = fields.length := by
  simp only [Core.scalarFieldsValid, Bool.and_eq_true, decide_eq_true_eq] at valid
  exact valid.1.1

/-- Protected reusable relation: unlike a raw self-referential projection
    equality, simplification does not unfold this into a cyclic rewrite. -/
def AggregateRepresents (cell : Option Core.Value) (fields : List UInt64) : Prop :=
  cell = some (.scalarAggregate fields)
@[grind] theorem aggregate_represents_eq (cell : Option Core.Value) (fields : List UInt64)
    (known : AggregateRepresents cell fields) : cell = some (.scalarAggregate fields) := known

@[grind] theorem aggregate_cell (shape : List Element) (widths : List Nat) (cell : Option Core.Value)
    (typed : CellValid (.record shape) cell) (initialized : cell.isSome = true)
    (descriptor : Element.widthsFuel 65 (.record shape) = some widths) :
    cell = some (.scalarAggregate (aggregateFields cell)) ∧
    Core.scalarFieldsValid widths (aggregateFields cell) = true := by
  cases cell with
  | none => simp at initialized
  | some value =>
    obtain ⟨fields, same, observedWidths, sameWidths, valid⟩ := typed value rfl
    subst value
    rw [descriptor] at sameWidths
    cases sameWidths
    exact ⟨rfl, valid⟩

@[grind] theorem aggregate_widths (shape : List Element) (widths : List Nat) (cell : Option Core.Value)
    (typed : CellValid (.record shape) cell) (initialized : cell.isSome = true)
    (descriptor : Element.widthsFuel 65 (.record shape) = some widths) :
    Core.scalarFieldsValid widths (aggregateFields cell) = true :=
  (aggregate_cell shape widths cell typed initialized descriptor).2

@[grind] theorem aggregate_rebuild (shape : List Element) (widths : List Nat) (cell : Option Core.Value)
    (typed : CellValid (.record shape) cell) (initialized : cell.isSome = true)
    (descriptor : Element.widthsFuel 65 (.record shape) = some widths) :
    cell = some (.scalarAggregate (projectFields (aggregateFields cell) widths.length)) := by
  obtain ⟨same, valid⟩ := aggregate_cell shape widths cell typed initialized descriptor
  have length := scalar_fields_length widths (aggregateFields cell) valid
  rw [length, projectFields_eq]
  exact same

@[grind] theorem aggregate_field_bound (widths : List Nat) (fields : List UInt64) (index width : Nat)
    (valid : Core.scalarFieldsValid widths fields = true) (descriptor : widths[index]? = some width) :
    (fields[index]?.getD 0).toNat < Scalar.bound width := by
  have length := scalar_fields_length widths fields valid
  have all : (widths.zip fields).all (fun (w,v) => decide (v.toNat < Scalar.bound w)) = true := by
    simp only [Core.scalarFieldsValid, Bool.and_eq_true] at valid
    exact valid.2
  clear valid
  induction widths generalizing fields index with
  | nil => simp at descriptor
  | cons head widths ih =>
    cases fields with
    | nil => simp at length
    | cons field fields =>
      simp only [List.zip_cons_cons, List.all_cons, Bool.and_eq_true] at all
      cases index with
      | zero =>
        simp only [List.getElem?_cons_zero, Option.some.injEq] at descriptor
        subst head
        simpa using all.1
      | succ index =>
        -- `generalizing` reverts dependents in context order: `descriptor` (an argument) precedes `length` (a `have`).
        apply ih fields index (by simpa using descriptor) (by simpa using length) all.2
end Witchy.Storage

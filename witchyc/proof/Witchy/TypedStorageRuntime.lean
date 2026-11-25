import Witchy.CoreStep
import Witchy.StorageLayout

namespace Witchy.Storage

/-- Storage descriptors: a numeric scalar is exactly its checked width in
    bytes (`Storage.Scalar`); records preserve the source's ordered nested
    shape. Resource cells keep the generic Lean representation. -/
inductive Element where
  | resource | scalar (width : Nat) | record (fields : List Element)
  deriving Repr

mutual
  def elementDecEq : (a b : Element) → Decidable (a = b)
    | .resource, .resource => .isTrue rfl
    | .scalar x, .scalar y => if h : x = y then .isTrue (congrArg Element.scalar h) else .isFalse (fun same => h (Element.scalar.inj same))
    | .record xs, .record ys => match elementListDecEq xs ys with
      | .isTrue h => .isTrue (congrArg Element.record h)
      | .isFalse h => .isFalse (fun same => h (Element.record.inj same))
    | .resource, .scalar _ | .resource, .record _
      | .scalar _, .resource | .scalar _, .record _
      | .record _, .resource | .record _, .scalar _ => .isFalse (by intro h; cases h)
  termination_by a b => sizeOf a + sizeOf b
  decreasing_by all_goals simp_wf <;> omega
  def elementListDecEq : (xs ys : List Element) → Decidable (xs = ys)
    | [], [] => .isTrue rfl
    | [], _::_ | _::_, [] => .isFalse (by intro h; cases h)
    | x::xs, y::ys => match elementDecEq x y, elementListDecEq xs ys with
      | .isTrue h, .isTrue t => .isTrue (by cases h; cases t; rfl)
      | .isFalse h, _ => .isFalse (fun same => h (List.cons.inj same).1)
      | _, .isFalse h => .isFalse (fun same => h (List.cons.inj same).2)
  termination_by xs ys => sizeOf xs + sizeOf ys
  decreasing_by all_goals simp_wf <;> omega
end
instance : DecidableEq Element := elementDecEq

/-- Ordered leaf widths; these are the `widths` operand of aggregate transfers. -/
def Element.widthsFuel : Nat → Element → Option (List Nat)
  | 0, _ => none
  | _+1, .scalar width => if Scalar.wellFormed width then some [width] else none
  | fuel+1, .record fields => do
    if fields.isEmpty then none else pure ()
    return (← fields.mapM (Element.widthsFuel fuel)).flatten
  | _, _ => none

def Element.layoutFuel : Nat → Element → Option Layout
  | 0, _ => none
  | _+1, .scalar width =>
    if Scalar.wellFormed width then some ⟨UInt64.ofNat width, UInt64.ofNat width, UInt64.ofNat width⟩ else none
  | _+1, .resource => some ⟨8,8,8⟩
  | fuel+1, .record fields => do
    let widths ← Element.widthsFuel (fuel+1) (.record fields)
    if widths.length > 4096 then none else pure ()
    let layouts ← fields.mapM (Element.layoutFuel fuel)
    let (size, alignment) := layouts.foldl (fun (size, alignment) field =>
      let a := field.alignment.toNat
      (((size+a-1)/a)*a+field.size.toNat, max alignment a)) (0,1)
    let size := ((size+alignment-1)/alignment)*alignment
    if size = 0 ∨ size > 32768 then none else
      some ⟨UInt64.ofNat size, UInt64.ofNat size, UInt64.ofNat alignment⟩

def Element.layout (element : Element) : Layout :=
  (element.layoutFuel 65).getD ⟨0,0,1⟩
/-- Ordered scalar leaves as (offset, width), including every nested record's padding boundaries. -/
def Element.leavesFuel : Nat → Element → Option (List (Nat × Nat))
  | 0, _ => none
  | _+1, .scalar width => if Scalar.wellFormed width then some [(0,width)] else none
  | fuel+1, .record fields => do
    if fields.isEmpty then none else pure ()
    let children ← fields.mapM (fun field => do
      let layout ← Element.layoutFuel fuel field
      let leaves ← Element.leavesFuel fuel field
      pure (layout,leaves))
    let (_, leaves) := children.foldl (fun (cursor, out) (layout, leaves) =>
      let a := layout.alignment.toNat
      let offset := ((cursor+a-1)/a)*a
      (offset+layout.size.toNat,out ++ leaves.map (fun (field,width) => (offset+field,width)))) (0,[])
    return leaves
  | _, _ => none

def Element.fieldsWellFormed (element : Element) : Bool :=
  match element with
  | .resource => true
  | _ => match element.leavesFuel 65, element.widthsFuel 65 with
    | some leaves, some widths =>
      decide (leaves.map Prod.snd = widths) &&
      leaves.all (fun (offset,width) => Scalar.wellFormed width && decide (offset + width ≤ element.layout.size.toNat) && decide (offset % width = 0)) &&
      (leaves.zip (leaves.drop 1)).all (fun ((offset,width),(next,_)) => decide (offset + width ≤ next))
    | _, _ => false

def Element.properties (element : Element) : Prop :=
  element.layout.size.toNat ≤ element.layout.stride.toNat ∧
  element.layout.stride.toNat % element.layout.alignment.toNat = 0 ∧
  Scalar.wellFormed element.layout.alignment.toNat = true ∧
  element.layout.stride.toNat ≤ 32768
instance (element : Element) : Decidable element.properties := inferInstanceAs (Decidable (_ ∧ _ ∧ _ ∧ _))
def Element.nodesFuel : Nat → Element → Nat
  | 0, _ => 4097
  | fuel+1, .record fields => 1 + (fields.map (Element.nodesFuel fuel)).sum
  | _+1, _ => 1
def Element.wellFormed (element : Element) : Bool :=
  decide element.properties && ((element.layoutFuel 65).isSome && decide (element.nodesFuel 65 ≤ 4096) && element.fieldsWellFormed)

theorem Element.valid (element : Element) (valid : element.wellFormed = true) : element.layout.Valid := by
  have h : element.properties := by
    simp only [Element.wellFormed, Bool.and_eq_true, decide_eq_true_eq] at valid
    exact valid.1
  exact ⟨h.1, h.2.1, Scalar.wellFormed_alignment _ h.2.2.1⟩

/-- Pure allocation has exactly the original logical cells. Native allocation
    receives the complete descriptor plus its canonical layout, recomputes the
    physical plan for every element kind through one path, and returns none on
    mismatch or physical refusal before publishing a block. -/
@[extern "witchy_block_allocate_element"]
def allocateBlock (count : @& Nat) (_element : @& Element) (_size _stride _alignment : UInt64) : Option (Block Core.Value) :=
  some (Block.mk (List.replicate count none))

def allocate (s : Memory.StateOf Core.Value) (count : Nat) (element : Element) :
    Memory.StateOf Core.Value × Memory.Result Memory.Handle :=
  if s.blocks.length < s.limits.allocations then
    if count ≤ s.available then
      match allocateBlock count element element.layout.size element.layout.stride element.layout.alignment with
      | none => (s, .error .allocation)
      | some block =>
        ({ s with blocks := s.blocks ++ [some block], available := s.available - count }, .ok s.blocks.length)
    else (s, .error .allocation)
  else (s, .error .metadata)

theorem allocate_eq (s : Memory.StateOf Core.Value) (count : Nat) (element : Element) :
    allocate s count element = Memory.allocate s count := by
  simp only [allocate, allocateBlock, Memory.allocate]

/-- Canonical descriptors describe bytes without changing logical quota units. -/
theorem allocation_bytes_bound (element : Element) (count : Nat) (bounded : count ≤ 65536)
    (valid : element.wellFormed = true) : element.layout.stride.toNat * count < 2^64 := by
  have h : element.properties := by
    simp only [Element.wellFormed, Bool.and_eq_true, decide_eq_true_eq] at valid
    exact valid.1
  have product := Nat.mul_le_mul h.2.2.2 bounded
  omega

/-- Range facts for narrow source interfaces and initialized scalar cells:
    every stored number fits its descriptor's bound. -/
def ValueValid (element : Element) (value : Core.Value) : Prop :=
  match element with
  | .scalar width => ∃ number : UInt64, value = .number number ∧ number.toNat < Scalar.bound width
  | .resource => True
  | .record fields => ∃ values : List UInt64, value = .scalarAggregate values ∧
      ∃ widths, Element.widthsFuel 65 (.record fields) = some widths ∧ Core.scalarFieldsValid widths values = true

def CellsValid (element : Element) (block : Block Core.Value) : Prop :=
  ∀ value, some value ∈ block.slots → ValueValid element value

@[simp] def CellValid (element : Element) (cell : Option Core.Value) : Prop :=
  ∀ value, cell = some value → ValueValid element value

@[grind] theorem cells_option (element : Element) (block : Block Core.Value) (index : Nat)
    (valid : CellsValid element block) : CellValid element ((block.slots[index]?).bind id) := by
  intro value observed
  cases found : block.slots[index]? with
  | none => simp [found] at observed
  | some cell =>
    cases cell with
    | none => simp [found] at observed
    | some payload =>
      simp only [found, Option.bind_some, id_eq, Option.some.injEq] at observed
      subst value
      exact valid payload (List.mem_of_getElem? found)

@[grind] theorem cells_set_cell (element : Element) (block : Block Core.Value) (index : Nat) (cell : Option Core.Value)
    (valid : CellsValid element block) (item : CellValid element cell) :
    CellsValid element (Block.mk (block.slots.set index cell)) := by
  intro value found
  rcases List.mem_or_eq_of_mem_set found with old | same
  · exact valid value old
  · exact item value same.symm

@[simp] theorem cells_nil (element : Element) : CellsValid element (Block.mk []) := by
  intro value found; cases found

@[simp] theorem cells_cons (element : Element) (head : Option Core.Value) (tail : List (Option Core.Value)) :
    CellsValid element (Block.mk (head :: tail)) ↔
      (∀ value, some value = head → ValueValid element value) ∧ CellsValid element (Block.mk tail) := by
  simp only [CellsValid, Block.slots_mk, List.mem_cons, or_imp, forall_and]

@[simp] theorem cells_empty (element : Element) (count : Nat) :
    CellsValid element (Block.mk (List.replicate count none)) := by
  intro value found
  simp at found

@[simp] theorem cells_replicate (element : Element) (count : Nat) :
    CellsValid element (Array.replicate count none) := by
  intro value found
  simp [Block.slots] at found

@[grind] theorem cells_get (element : Element) (block : Block Core.Value) (index : Nat) (value : Core.Value)
    (valid : CellsValid element block) (found : block.slots[index]? = some (some value)) :
    ValueValid element value := valid value (List.mem_of_getElem? found)

@[grind] theorem cells_set_none (element : Element) (block : Block Core.Value) (index : Nat)
    (valid : CellsValid element block) : CellsValid element (Block.mk (block.slots.set index none)) := by
  intro value found
  rcases List.mem_or_eq_of_mem_set found with old | bad
  · exact valid value old
  · cases bad

@[grind] theorem cells_set_some (element : Element) (block : Block Core.Value) (index : Nat) (value : Core.Value)
    (valid : CellsValid element block) (item : ValueValid element value) :
    CellsValid element (Block.mk (block.slots.set index (some value))) := by
  intro other found
  rcases List.mem_or_eq_of_mem_set found with old | same
  · exact valid other old
  · cases same; exact item

/-- Total logical projection. Every use as a number must first prove the
    observed cell equals its numeric constructor; no default is assumed. -/
@[simp] def numericCell : Option Core.Value → UInt64
  | some (.number value) => value
  | _ => 0

@[grind] theorem scalar_cell (width : Nat) (block : Block Core.Value) (index : Nat)
    (valid : CellsValid (.scalar width) block) (initialized : ((block.slots[index]?).bind id).isSome = true) :
    (block.slots[index]?).bind id = some (.number (numericCell ((block.slots[index]?).bind id))) ∧
      (numericCell ((block.slots[index]?).bind id)).toNat < Scalar.bound width := by
  cases found : block.slots[index]? with
  | none => simp [found] at initialized
  | some cell =>
    cases cell with
    | none => simp [found] at initialized
    | some value =>
      obtain ⟨number, same, bound⟩ := cells_get (.scalar width) block index value valid found
      subst value
      simp [bound]


end Witchy.Storage

namespace Witchy.Storage

/-- Pure representation checks erase; native checks the private physical header
    and payload codec before memory mutation, for every element kind through one
    descriptor comparison. Logical source refinements and checked code/table
    binding separately establish these preconditions. -/
@[extern "witchy_block_matches_element"]
def blockMatchesElement (_block : @& Block Core.Value) (_element : @& Element) : Bool := true

@[extern "witchy_block_accepts"]
def blockAccepts (_block : @& Block Core.Value) (_value : @& Core.Value) : Bool := true

@[simp] theorem blockMatchesElement_eq (block : Block Core.Value) (element : Element) : blockMatchesElement block element = true := rfl

/-- A slot list whose every cell is present is initialized at each index. The
    premise is a closed computation over the actual cells, so a list with any
    empty cell cannot use this rule. -/
theorem all_initialized {α : Type} (slots : List (Option α)) (index : Nat) (bound : index < slots.length)
    (full : slots.all Option.isSome = true) : slots[index].isSome = true :=
  List.all_eq_true.mp full _ (List.getElem_mem bound)
end Witchy.Storage

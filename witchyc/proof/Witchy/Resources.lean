import Witchy.Core
import Witchy.Inventory

namespace Witchy.Core

variable {Cell : Type}

/-- Memory resources use distinct namespaces; equal numeric allocation/loan IDs
    do not denote the same resource. Capabilities have a separate authority law. -/
inductive Resource where
  | allocation (handle : Memory.Handle)
  | loan (identity : Memory.LoanId)
  deriving Repr, DecidableEq

def valueResource : Value → Option Resource
  | .owner handle => some (.allocation handle)
  | .reference identity => some (.loan identity)
  | _ => none

def weight (resource : Resource) (cell : Option Value) : Nat :=
  if cell.bind valueResource = some resource then 1 else 0

def registerCount (registers : List (Option Value)) (resource : Resource) : Nat :=
  (registers.map (weight resource)).sum

def memoryCount (memory : Memory.StateOf α) : Resource → Nat
  | .allocation handle => if (Memory.block? memory handle).isSome then 1 else 0
  | .loan identity => if (Memory.loan? memory identity).isSome then 1 else 0

/-- The scalar kernel stores no resources inside cells. Every live allocation
    and loan therefore has exactly one corresponding register, and vice versa. -/
def OwnsResources (s : State) : Prop :=
  ∀ resource, registerCount s.registers resource = memoryCount s.memory resource

theorem present_resource_count_positive (registers : List (Option Value)) (index : Nat)
    (value : Value) (resource : Resource) (present : registers[index]? = some (some value))
    (identity : valueResource value = some resource) : 1 ≤ registerCount registers resource := by
  induction registers generalizing index with
  | nil => simp at present
  | cons head tail ih =>
    cases index with
    | zero =>
      simp only [List.getElem?_cons_zero, Option.some.injEq] at present
      subst head
      simp [registerCount, weight, identity]
    | succ index =>
      have positive := ih index (by simpa using present)
      simp only [registerCount, List.map_cons, List.sum_cons] at positive ⊢
      omega

theorem memory_count_at_most_one (memory : Memory.StateOf α) (resource : Resource) :
    memoryCount memory resource ≤ 1 := by
  cases resource with
  | allocation handle => cases hb : Memory.block? memory handle <;> simp [memoryCount, hb]
  | loan identity => cases hl : Memory.loan? memory identity <;> simp [memoryCount, hl]

/-- A consequence of the proposed invariant, not its preservation theorem. -/
theorem owned_resource_at_most_one (s : State) (owned : OwnsResources s)
    (resource : Resource) : registerCount s.registers resource ≤ 1 := by
  rw [owned resource]
  exact memory_count_at_most_one s.memory resource

theorem register_set_accounting (registers : List (Option Value)) (index : Nat)
    (old new : Option Value) (resource : Resource) (lookup : registers[index]? = some old) :
    registerCount (registers.set index new) resource + weight resource old =
      registerCount registers resource + weight resource new := by
  exact Storage.sum_set_weight registers index old new (weight resource) lookup

theorem register_set_nonresource (registers : List (Option Value)) (index : Nat)
    (old new : Option Value) (resource : Resource) (lookup : registers[index]? = some old)
    (oldEmpty : weight resource old = 0) (newEmpty : weight resource new = 0) :
    registerCount (registers.set index new) resource = registerCount registers resource := by
  have equation := register_set_accounting registers index old new resource lookup
  simpa only [oldEmpty, newEmpty, Nat.add_zero] using equation

theorem get_register_lookup (s : StateOf Cell) (index : Nat) (value : Value)
    (found : get s index = .ok value) : s.registers[index]? = some (some value) := by
  cases cell : s.registers[index]? with
  | none => simp [get, cell] at found
  | some slot =>
    cases slot with
    | none => simp [get, cell] at found
    | some old =>
      have equal : old = value := by simpa [get, cell] using found
      simp [equal]

theorem vacant_register_lookup (s : StateOf Cell) (index : Nat) (empty : vacant s index = .ok ()) :
    s.registers[index]? = some none := by
  cases cell : s.registers[index]? with
  | none => simp [vacant, cell] at empty
  | some slot =>
    cases slot with
    | none => rfl
    | some value => simp [vacant, cell] at empty

theorem put_shape (s next : StateOf Cell) (index : Nat) (value : Value)
    (accepted : put s index value = .ok next) :
    s.registers[index]? = some none ∧ next = { s with registers := s.registers.set index (some value) } := by
  cases empty : vacant s index with
  | error reason => simp [put, empty, Bind.bind] at accepted
  | ok _unused =>
    have vacant := vacant_register_lookup s index (by simpa using empty)
    have equal : { s with registers := s.registers.set index (some value) } = next := by
      simpa only [put, empty, Bind.bind, Pure.pure, Result.ok.injEq] using accepted
    exact ⟨vacant, equal.symm⟩

theorem put_register_count (s next : StateOf Cell) (index : Nat) (value : Value) (resource : Resource)
    (accepted : put s index value = .ok next) :
    registerCount next.registers resource = registerCount s.registers resource + weight resource (some value) := by
  obtain ⟨empty, shape⟩ := put_shape s next index value accepted
  subst next
  simpa [weight] using register_set_accounting s.registers index none (some value) resource empty

theorem putCopy_register_count (s next : StateOf Cell) (index : Nat) (value : Value) (resource : Resource)
    (accepted : putCopy s index value = .ok next) :
    registerCount next.registers resource = registerCount s.registers resource := by
  cases value <;> unfold putCopy at accepted
  all_goals split at accepted <;> cases accepted
  all_goals try simp_all
  all_goals
    apply register_set_nonresource
    · assumption
    · simp [weight, valueResource]
    · simp [weight, valueResource]

theorem move_register_count (registers : List (Option Value)) (dst src : Nat) (value : Value)
    (resource : Resource) (empty : registers[dst]? = some none)
    (present : registers[src]? = some (some value)) :
    registerCount ((registers.set dst (some value)).set src none) resource =
      registerCount registers resource := by
  have different : dst ≠ src := by
    intro equal
    subst dst
    rw [empty] at present
    cases present
  have kept : (registers.set dst (some value))[src]? = some (some value) := by
    simpa [List.getElem?_set_ne different] using present
  have added := register_set_accounting registers dst none (some value) resource empty
  have removed := register_set_accounting (registers.set dst (some value)) src (some value) none resource kept
  simp only [weight, Option.bind_none, reduceCtorEq, ↓reduceIte, Nat.add_zero] at added removed
  omega

end Witchy.Core

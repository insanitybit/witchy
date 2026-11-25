import Witchy.BorrowRoots
import Witchy.ScalarAggregateFrames
namespace Witchy.Core
theorem scalar_mapM_member (f : α → Result β) (xs : List α) (ys : List β)
    (accepted : xs.mapM f = .ok ys) (x : α) (member : x ∈ xs) : ∃ y, f x = .ok y := by
  have loop : ∀ (xs : List α) (acc ys : List β), List.mapM.loop f xs acc = .ok ys →
      ∀ x ∈ xs, ∃ y, f x = .ok y := by
    intro xs
    induction xs with
    | nil => intro acc ys accepted x member; simp at member
    | cons head tail ih =>
      intro acc ys accepted x member
      change (match f head with | .ok b => List.mapM.loop f tail (b::acc) | .error e => .error e) = .ok ys at accepted
      cases found : f head with
      | error reason => simp [found] at accepted
      | ok value =>
        simp only [found] at accepted
        rcases List.mem_cons.mp member with same | rest
        · subst x; exact ⟨value,found⟩
        · exact ih (value::acc) ys accepted x rest
  exact loop xs [] ys accepted x member

theorem pack_preflight (s next : StateOf Cell) (dst : Nat) (sources : List Nat) (widths : List Nat)
    (accepted : packRegisters s dst sources widths = .ok next) :
    vacant s dst = .ok () ∧ ∃ fields, sources.mapM (number s) = .ok fields := by
  unfold packRegisters at accepted
  split at accepted <;> simp only [Bind.bind, Pure.pure] at accepted
  · cases vacancy : vacant s dst with
    | error reason => simp [vacancy] at accepted
    | ok doneValue =>
      cases doneValue
      cases values : sources.mapM (number s) with
      | error reason => simp [vacancy,values] at accepted
      | ok fields => exact ⟨rfl,fields,rfl⟩
  · cases accepted

theorem unpack_preflight (s next : StateOf Cell) (destinations : List Nat) (src : Nat) (widths : List Nat)
    (accepted : unpackRegisters s destinations src widths = .ok next) :
    (∃ fields, get s src = .ok (.scalarAggregate fields)) ∧ ∃ units, destinations.mapM (vacant s) = .ok units := by
  unfold unpackRegisters at accepted
  split at accepted <;> simp only [Bind.bind, Pure.pure] at accepted
  · cases value : get s src with
    | error reason => simp [value] at accepted
    | ok payload =>
      simp only [value] at accepted
      cases payload <;> simp only at accepted
      all_goals try contradiction
      case scalarAggregate fields =>
        split at accepted
        · cases vacancies : destinations.mapM (vacant s) with
          | error reason => simp [vacancies] at accepted
          | ok units => exact ⟨⟨fields,rfl⟩,units,rfl⟩
        · cases accepted
  · cases accepted

theorem pack_loan_roots (s next : StateOf Value) (dst : Nat) (sources : List Nat) (widths : List Nat)
    (known : LoanRootsRegistered s.registers s.memory.loans) (accepted : packRegisters s dst sources widths = .ok next) :
    LoanRootsRegistered next.registers next.memory.loans := by
  obtain ⟨vacancy,fields,values⟩ := pack_preflight s next dst sources widths accepted
  rw [(pack_context s next dst sources widths accepted).1]
  intro loan member
  obtain ⟨index,bound,found⟩ := List.mem_iff_getElem.mp (known loan member)
  have observed := List.getElem?_eq_some_iff.mpr ⟨bound,found⟩
  have different : index ≠ dst := by
    intro same; subst index
    simp only [vacant,observed] at vacancy
    cases vacancy
  have outside : index ∉ sources := by
    intro member
    obtain ⟨value,numbered⟩ := scalar_mapM_member (number s) sources fields values index member
    simp [number,get,observed,Bind.bind] at numbered
  apply List.mem_of_getElem?
  rw [(pack_frame s next dst sources widths index ⟨different,outside⟩ accepted).1]
  exact observed

theorem unpack_loan_roots (s next : StateOf Value) (destinations : List Nat) (src : Nat) (widths : List Nat)
    (known : LoanRootsRegistered s.registers s.memory.loans) (accepted : unpackRegisters s destinations src widths = .ok next) :
    LoanRootsRegistered next.registers next.memory.loans := by
  obtain ⟨⟨fields,payload⟩,units,vacancies⟩ := unpack_preflight s next destinations src widths accepted
  rw [(unpack_context s next destinations src widths accepted).1]
  intro loan member
  obtain ⟨index,bound,found⟩ := List.mem_iff_getElem.mp (known loan member)
  have observed := List.getElem?_eq_some_iff.mpr ⟨bound,found⟩
  have different : index ≠ src := by
    intro same; subst index
    simp [get,observed] at payload
  have outside : index ∉ destinations := by
    intro member
    obtain ⟨doneValue,vacancy⟩ := scalar_mapM_member (vacant s) destinations units vacancies index member
    simp [vacant,observed] at vacancy
  apply List.mem_of_getElem?
  rw [(unpack_frame s next destinations src widths index ⟨different,outside⟩ accepted).1]
  exact observed
end Witchy.Core

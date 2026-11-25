import Witchy.CoreRegisterInvariant
import Witchy.ScalarAggregateRoots
namespace Witchy.Core
private def DataCell : Option Value → Prop
  | none | some (.number _) | some (.scalarAggregate _) => True
  | _ => False
private theorem data_weight (cell : Option Value) (valid : DataCell cell) (resource : Resource) : weight resource cell = 0 := by
  cases cell with
  | none => simp [weight]
  | some value => cases value <;> simp_all [DataCell,weight,valueResource]
private theorem data_caps (cell : Option Value) (valid : DataCell cell) (authority : Authority.State) :
    ∀ value, cell = some value → Storage.valueCapabilitiesValid authority value := by
  cases cell with
  | none => intro value impossible; cases impossible
  | some value => cases value <;> simp_all [DataCell,Storage.valueCapabilitiesValid]
private theorem data_not_owner (cell : Option Value) (valid : DataCell cell) (handle : Nat) : cell ≠ some (.owner handle) := by
  cases cell with
  | none => simp
  | some value => cases value <;> simp_all [DataCell]
private theorem set_data (s : StateOf Value) (index : Nat) (old replacement : Option Value)
    (lookup : s.registers[index]? = some old) (oldData : DataCell old) (newData : DataCell replacement)
    (known : StorageStateInvariant s) : StorageStateInvariant {s with registers := s.registers.set index replacement} := by
  refine ⟨?_,known.structural,⟨Storage.cells_caps_set _ _ _ _ known.capabilities.1 (data_caps _ newData _),known.capabilities.2⟩,?_⟩
  · intro resource
    change registerCount _ resource + _ = _
    rw [register_set_nonresource _ _ _ _ _ lookup (data_weight _ oldData _) (data_weight _ newData _)]
    exact known.inventory resource
  · intro loan member
    obtain ⟨position,bound,present⟩ := List.mem_iff_getElem.mp (known.roots loan member)
    have observed := List.getElem?_eq_some_iff.mpr ⟨bound,present⟩
    have separate : position ≠ index := by
      intro same; subst position
      rw [lookup] at observed
      have bad := Option.some.inj observed
      exact data_not_owner old oldData loan.target bad
    apply List.mem_of_getElem?
    rw [List.getElem?_set_ne (Ne.symm separate)]
    exact observed
private theorem updates_data (updates : List (Nat × Option Value)) (s : StateOf Value)
    (known : StorageStateInvariant s)
    (oldData : ∀ update ∈ updates, ∃ old, s.registers[update.1]? = some old ∧ DataCell old)
    (newData : ∀ update ∈ updates, DataCell update.2) :
    StorageStateInvariant {s with registers := updates.foldl (fun registers update => registers.set update.1 update.2) s.registers} := by
  induction updates generalizing s with
  | nil => exact known
  | cons head tail ih =>
    obtain ⟨old,lookup,valid⟩ := oldData head (by simp)
    have next := set_data s head.1 old head.2 lookup valid (newData head (by simp)) known
    apply ih {s with registers := s.registers.set head.1 head.2} next
    · intro update member
      by_cases same : head.1 = update.1
      · refine ⟨head.2,?_,newData head (by simp)⟩
        dsimp
        rw [←same]
        exact List.getElem?_set_self (List.getElem?_eq_some_iff.mp lookup).1
      · obtain ⟨old,found,valid⟩ := oldData update (by simp [member])
        exact ⟨old,by simpa only [List.getElem?_set_ne same] using found,valid⟩
    · intro update member; exact newData update (by simp [member])
theorem pack_storage (s next : StateOf Value) (dst : Nat) (sources : List Nat) (widths : List Nat)
    (known : StorageStateInvariant s) (accepted : packRegisters s dst sources widths = .ok next) : StorageStateInvariant next := by
  obtain ⟨vacancy,fields,values⟩ := pack_preflight s next dst sources widths accepted
  have outside : dst ∉ sources := by
    unfold packRegisters at accepted
    split at accepted
    · simp_all
    · cases accepted
  have cleared := updates_data (sources.map (fun index => (index,none))) s known (by
    intro update member
    obtain ⟨index,sourceMember,same⟩ := List.mem_map.mp member
    subst update
    obtain ⟨value,found⟩ := scalar_mapM_member (number s) sources fields values index sourceMember
    exact ⟨some (.number value),get_register_lookup s index _ (number_get_value s index value found),trivial⟩) (by
    intro update member
    obtain ⟨index,sourceMember,same⟩ := List.mem_map.mp member
    subst update; trivial)
  simp only [List.foldl_map] at cleared
  have final := set_data {s with registers := sources.foldl (fun registers index => registers.set index none) s.registers}
    dst none (some (.scalarAggregate fields)) (by
      dsimp
      rw [scalar_clear_frame _ _ _ outside]
      exact vacant_register_lookup s dst vacancy) trivial trivial cleared
  unfold packRegisters at accepted
  simp only [Bind.bind,Pure.pure,vacancy,values] at accepted
  repeat' first | split at accepted | cases accepted
  exact final
theorem unpack_storage (s next : StateOf Value) (destinations : List Nat) (src : Nat) (widths : List Nat)
    (known : StorageStateInvariant s) (accepted : unpackRegisters s destinations src widths = .ok next) : StorageStateInvariant next := by
  obtain ⟨⟨fields,payload⟩,units,vacancies⟩ := unpack_preflight s next destinations src widths accepted
  have outside : src ∉ destinations := by
    unfold unpackRegisters at accepted
    split at accepted
    · simp_all
    · cases accepted
  have cleared := set_data s src (some (.scalarAggregate fields)) none
    (get_register_lookup s src _ payload) trivial trivial known
  have final := updates_data ((destinations.zip fields).map (fun entry => (entry.1,some (.number entry.2))))
    {s with registers := s.registers.set src none} cleared (by
      intro update member
      obtain ⟨entry,entryMember,same⟩ := List.mem_map.mp member
      subst update
      have target : entry.1 ∈ destinations := (List.of_mem_zip entryMember).1
      obtain ⟨doneValue,vacancy⟩ := scalar_mapM_member (vacant s) destinations units vacancies entry.1 target
      cases doneValue
      refine ⟨none,?_,trivial⟩
      dsimp
      rw [List.getElem?_set_ne (by intro equal; exact outside (equal ▸ target))]
      exact vacant_register_lookup s entry.1 vacancy) (by
      intro update member
      obtain ⟨entry,entryMember,same⟩ := List.mem_map.mp member
      subst update; trivial)
  simp only [List.foldl_map] at final
  unfold unpackRegisters at accepted
  simp only [Bind.bind,Pure.pure,payload,vacancies] at accepted
  repeat' first | split at accepted | cases accepted
  exact final
end Witchy.Core

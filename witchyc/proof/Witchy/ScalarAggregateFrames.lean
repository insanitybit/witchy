import Witchy.CoreStep
namespace Witchy.Core
/-- A list preflight cannot invent a fault absent from each observed element. -/
theorem scalar_mapM_no_fault (f : α → Result β) (items : List α) (reason : Fault)
    (safe : ∀ item ∈ items, f item ≠ .error reason) : items.mapM f ≠ .error reason := by
  have loop : ∀ (items : List α) (acc : List β),
      (∀ item ∈ items, f item ≠ .error reason) → List.mapM.loop f items acc ≠ .error reason := by
    intro items
    induction items with
    | nil => intro acc safe; simp [List.mapM.loop]
    | cons head tail ih =>
      intro acc safe
      change (match f head with | .ok value => List.mapM.loop f tail (value :: acc) | .error e => .error e) ≠ _
      cases found : f head with
      | error e =>
        intro bad
        cases Result.error.inj bad
        exact safe head (by simp) found
      | ok value => exact ih (value :: acc) (fun item member => safe item (by simp [member]))
  exact loop items [] safe

theorem scalar_clear_frame (sources : List Nat) (registers : List (Option Value)) (index : Nat)
    (outside : index ∉ sources) :
    (sources.foldl (fun r i => r.set i none) registers)[index]? = registers[index]? := by
  induction sources generalizing registers with
  | nil => rfl
  | cons source sources ih =>
    simp only [List.mem_cons, not_or] at outside
    simp only [List.foldl_cons]
    rw [ih _ outside.2, List.getElem?_set_ne (Ne.symm outside.1)]
theorem scalar_fill_frame (destinations : List Nat) (fields : List UInt64) (registers : List (Option Value)) (index : Nat)
    (outside : index ∉ destinations) :
    ((destinations.zip fields).foldl (fun r (i,v) => r.set i (some (.number v))) registers)[index]? = registers[index]? := by
  induction destinations generalizing fields registers with
  | nil => rfl
  | cons destination destinations ih =>
    cases fields with
    | nil => rfl
    | cons field fields =>
      simp only [List.mem_cons, not_or] at outside
      simp only [List.zip_cons_cons, List.foldl_cons]
      rw [ih _ _ outside.2, List.getElem?_set_ne (Ne.symm outside.1)]
private theorem update_length (items : List α) (index : α → Nat) (value : α → Option Value)
    (registers : List (Option Value)) :
    (items.foldl (fun registers item => registers.set (index item) (value item)) registers).length = registers.length := by
  induction items generalizing registers with
  | nil => rfl
  | cons item items ih =>
    simp only [List.foldl_cons]
    rw [ih,List.length_set]
theorem pack_length (s next : StateOf Cell) (dst : Nat) (sources : List Nat) (widths : List Nat)
    (accepted : packRegisters s dst sources widths = .ok next) : next.registers.length = s.registers.length := by
  unfold packRegisters at accepted
  simp only [Bind.bind,Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  simp only [List.length_set,update_length]
theorem unpack_length (s next : StateOf Cell) (destinations : List Nat) (src : Nat) (widths : List Nat)
    (accepted : unpackRegisters s destinations src widths = .ok next) : next.registers.length = s.registers.length := by
  unfold unpackRegisters at accepted
  simp only [Bind.bind,Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  simp only [List.length_set,update_length]
theorem pack_frame (s next : StateOf Cell) (dst : Nat) (sources : List Nat) (widths : List Nat) (index : Nat)
    (outside : index ≠ dst ∧ index ∉ sources) (accepted : packRegisters s dst sources widths = .ok next) :
    next.registers[index]? = s.registers[index]? ∧ next.memory = s.memory ∧ next.authority = s.authority := by
  unfold packRegisters at accepted
  simp only [Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  rw [List.getElem?_set_ne (Ne.symm outside.1), scalar_clear_frame _ _ _ outside.2]
  exact ⟨rfl,rfl,rfl⟩
theorem unpack_frame (s next : StateOf Cell) (destinations : List Nat) (src : Nat) (widths : List Nat) (index : Nat)
    (outside : index ≠ src ∧ index ∉ destinations) (accepted : unpackRegisters s destinations src widths = .ok next) :
    next.registers[index]? = s.registers[index]? ∧ next.memory = s.memory ∧ next.authority = s.authority := by
  unfold unpackRegisters at accepted
  simp only [Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  rw [scalar_fill_frame _ _ _ _ outside.2, List.getElem?_set_ne (Ne.symm outside.1)]
  exact ⟨rfl,rfl,rfl⟩
theorem put_memory (s next : StateOf Cell) (dst : Nat) (value : Value)
    (accepted : put s dst value = .ok next) : next.memory = s.memory := by
  simp only [put, Bind.bind] at accepted
  split at accepted <;> cases accepted
  rfl
theorem copy_memory (s next : StateOf Cell) (dst : Nat) (value : Value)
    (accepted : putCopy s dst value = .ok next) : next.memory = s.memory := by
  unfold putCopy at accepted
  split at accepted <;> cases accepted
  all_goals rfl
-- No choice of an outside register is needed for memory/authority preservation.
theorem pack_context (s next : StateOf Cell) (dst : Nat) (sources : List Nat) (widths : List Nat)
    (accepted : packRegisters s dst sources widths = .ok next) : next.memory = s.memory ∧ next.authority = s.authority := by
  unfold packRegisters at accepted
  simp only [Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  exact ⟨rfl,rfl⟩
theorem unpack_context (s next : StateOf Cell) (destinations : List Nat) (src : Nat) (widths : List Nat)
    (accepted : unpackRegisters s destinations src widths = .ok next) : next.memory = s.memory ∧ next.authority = s.authority := by
  unfold unpackRegisters at accepted
  simp only [Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  exact ⟨rfl,rfl⟩
end Witchy.Core

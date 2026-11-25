import Witchy.CoreStoreTally
import Witchy.CoreMemoryEvidence
import Witchy.CoreMoveTally
import Witchy.CleanupCoreTally
import Witchy.CoreBooleanEffects

namespace Witchy.Core

/-- Exact physical token multiplicity and its authority table, including tokens
    in every initialized heap cell. Validity alone does not imply this property. -/
def CapabilityConserved (before after : StateOf Value) : Prop :=
  after.authority = before.authority ∧ ∀ token,
    statePayloadTally (Storage.capabilityWeight token) after =
      statePayloadTally (Storage.capabilityWeight token) before

theorem CapabilityConserved.refl (s : StateOf Value) : CapabilityConserved s s :=
  ⟨rfl, fun _ => rfl⟩

theorem CapabilityConserved.trans (s middle next : StateOf Value)
    (first : CapabilityConserved s middle) (second : CapabilityConserved middle next) :
    CapabilityConserved s next :=
  ⟨second.1.trans first.1, fun token => (second.2 token).trans (first.2 token)⟩

theorem put_capability_tally (s next : StateOf Value) (dst : Nat) (value : Value)
    (accepted : put s dst value = .ok next) (token : Nat) :
    next.authority = s.authority ∧
    statePayloadTally (Storage.capabilityWeight token) next =
      statePayloadTally (Storage.capabilityWeight token) s + Storage.capabilityWeight token value := by
  obtain ⟨empty, shape⟩ := put_shape s next dst value accepted
  subst next
  have equation := Storage.payload_tally_set (Storage.capabilityWeight token) s.registers dst none (some value) empty
  simp only [Option.elim_none, Option.elim_some, Nat.add_zero] at equation
  refine ⟨rfl, ?_⟩
  simp only [statePayloadTally, equation]
  omega

private theorem replace_zero_tally (weight : Value → Nat) (registers : List (Option Value)) (dst : Nat)
    (old replacement : Option Value) (present : registers[dst]? = some old)
    (oldZero : old.elim 0 weight = 0) (newZero : replacement.elim 0 weight = 0) :
    Storage.payloadTally weight (registers.set dst replacement) = Storage.payloadTally weight registers := by
  have equation := Storage.payload_tally_set weight registers dst old replacement present
  simpa only [oldZero, newZero, Nat.add_zero] using equation

theorem put_copy_capabilities (s next : StateOf Value) (dst : Nat) (value : Value)
    (accepted : putCopy s dst value = .ok next) : CapabilityConserved s next := by
  unfold putCopy at accepted
  split at accepted <;> cases accepted
  all_goals
    refine ⟨rfl, ?_⟩
    intro token
    dsimp only [statePayloadTally]
    apply congrArg (fun count => count + Storage.heapPayloadTally (Storage.capabilityWeight token) s.memory)
    apply replace_zero_tally (Storage.capabilityWeight token) s.registers dst _ _ (by assumption)
    all_goals simp only [Option.elim_none, Option.elim_some, Storage.capabilityWeight,
      reduceCtorEq, ↓reduceIte]

theorem with_number_capabilities (s : StateOf Value) (dst : Nat) (value : UInt64)
    (slot : NumberSlot s dst) : CapabilityConserved s (withNumber s dst value) :=
  put_copy_capabilities _ _ _ _ (put_number_effect s dst value slot)

theorem with_bool_capabilities (s : StateOf Value) (dst : Nat) (value : Bool)
    (slot : BoolSlot s dst) : CapabilityConserved s (withBool s dst value) :=
  put_copy_capabilities _ _ _ _ (put_bool_effect s dst value slot)

theorem move_step_capabilities (s next : StateOf Value) (dst src : Nat)
    (accepted : step s (.move dst src) = .ok next) : CapabilityConserved s next := by
  cases found : get s src with
  | error reason => simp [step, found, Bind.bind] at accepted
  | ok value =>
    cases inserted : put s dst value with
    | error reason =>
      cases value <;> simp_all [step, Bind.bind]
      split at accepted <;> simp_all
    | ok middle =>
      have shape : next = { middle with registers := middle.registers.set src none } := by
        cases value <;> simp_all [step, Bind.bind, Pure.pure]
        split at accepted <;> simp_all
      obtain ⟨empty, middleShape⟩ := put_shape s middle dst value inserted
      subst next
      subst middle
      refine ⟨rfl, ?_⟩
      intro token
      simp only [statePayloadTally, move_register_payload_tally (Storage.capabilityWeight token)
        s.registers dst src value empty (get_register_lookup s src value found)]

theorem end_loan_step_capabilities (s next : StateOf Value) (src : Nat)
    (accepted : step s (.endLoan src) = .ok next) : CapabilityConserved s next := by
  simp only [step, Bind.bind, Pure.pure, fromMemory] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    refine ⟨rfl, ?_⟩
    intro token
    have equation := Storage.payload_tally_set (Storage.capabilityWeight token) s.registers src
      (some (.reference _)) none (get_register_lookup _ _ _ (by assumption))
    have blocks (identity : Nat) : (Memory.endLoan s.memory identity).1.blocks = s.memory.blocks := by
      unfold Memory.endLoan
      split <;> rfl
    have registers : Storage.payloadTally (Storage.capabilityWeight token) (s.registers.set src none) =
        Storage.payloadTally (Storage.capabilityWeight token) s.registers := by
      simpa only [Option.elim_some, Option.elim_none, Storage.capabilityWeight, reduceCtorEq,
        ↓reduceIte, Nat.add_zero] using equation
    simpa only [statePayloadTally, Storage.heapPayloadTally, blocks] using
      congrArg (fun count => count + Storage.heapPayloadTally (Storage.capabilityWeight token) s.memory) registers

theorem borrow_step_capabilities (s next : StateOf Value) (dst src : Nat) (exclusive : Bool)
    (accepted : step s (.borrow dst src exclusive) = .ok next) : CapabilityConserved s next := by
  simp only [step, Bind.bind, fromMemory] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    have authority := (put_capability_tally _ _ _ _ accepted 0).1
    refine ⟨authority, ?_⟩
    intro token
    have current := (put_capability_tally _ _ _ _ accepted token).2
    have blocks (handle : Nat) : (Memory.borrow s.memory handle exclusive).1.blocks = s.memory.blocks := by
      unfold Memory.borrow
      repeat' first | rfl | split
    simpa only [statePayloadTally, Storage.heapPayloadTally, blocks,
      Storage.capabilityWeight, reduceCtorEq, ↓reduceIte, Nat.add_zero] using current

private theorem put_taken_capabilities (s next : StateOf Value) (dst index : Nat)
    (permission : Memory.Access) (value : Value)
    (taken : (Memory.take s.memory permission index).2 = .ok value)
    (inserted : put { s with memory := (Memory.take s.memory permission index).1 } dst value = .ok next) :
    CapabilityConserved s next := by
  refine ⟨(put_capability_tally _ _ _ _ inserted 0).1, ?_⟩
  intro token
  have registers := (put_capability_tally _ _ _ _ inserted token).2
  have heap := Storage.take_payload_tally (Storage.capabilityWeight token) s.memory permission index value taken
  dsimp only [statePayloadTally] at registers ⊢
  omega

/-- Accepted consuming extraction transfers its exact payload contribution
    from heap to register without assuming initial ownership or validity. -/
theorem take_move_step_capabilities (s next : StateOf Value) (dst src index : Nat)
    (accepted : step s (.takeMove dst src index) = .ok next) : CapabilityConserved s next := by
  simp only [step, StorageCell.ownedCells, StorageCell.decodeValue, ↓reduceIte, Bind.bind] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    apply put_taken_capabilities s next _ _ _ _ ?_ accepted
    exact from_memory_ok _ _ (by assumption)

/-- Consuming initialization preserves exact capability multiplicity, including
    tokens stored as heap payloads. -/
theorem store_move_step_capabilities (s next : StateOf Value) (dst index source : Nat)
    (accepted : step s (.storeMove dst index source) = .ok next) : CapabilityConserved s next := by
  obtain ⟨authority, tally⟩ := store_move_step_tally s next dst index source accepted
  exact ⟨authority, fun token => tally (Storage.capabilityWeight token)⟩

/-- Operations whose accepted steps only transfer existing capabilities. Drop,
    release, narrowing and external effects deliberately need other contracts. -/
def Instruction.transfersCapabilities : Instruction → Bool
  | .constant _ _ | .boolean _ _ | .copy _ _ | .calculate _ _ _ _
  | .move _ _ | .borrow _ _ _ | .endLoan _ | .takeMove _ _ _ | .storeMove _ _ _
  | .jump _ | .branchFalse _ _ => true
  | _ => false

theorem transfer_step_capabilities (s next : StateOf Value) (instruction : Instruction)
    (checked : instruction.transfersCapabilities = true)
    (accepted : step s instruction = .ok next) : CapabilityConserved s next := by
  cases instruction <;> simp only [Instruction.transfersCapabilities, Bool.false_eq_true] at checked
  all_goals first
    | contradiction
    | exact move_step_capabilities _ _ _ _ accepted
    | exact borrow_step_capabilities _ _ _ _ _ accepted
    | exact end_loan_step_capabilities _ _ _ accepted
    | exact take_move_step_capabilities _ _ _ _ _ accepted
    | exact store_move_step_capabilities _ _ _ _ _ accepted
    | simp only [step, Bind.bind] at accepted
  all_goals repeat' first
    | exact put_copy_capabilities _ _ _ _ accepted
    | split at accepted
    | cases accepted

end Witchy.Core

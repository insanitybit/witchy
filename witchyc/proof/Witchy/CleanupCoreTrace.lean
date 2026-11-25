import Witchy.CleanupCoreAction
import Witchy.RecursiveCleanupFuel

namespace Witchy.Core

/-- A finite emitted cleanup region with incoming witnesses for each selected
    action and an exact fetch witness at each address. Dynamic work expansion
    is represented explicitly; completion requires an empty pending list.
    Constructing this witness from source lowering remains an obligation. -/
inductive CleanupCoreTrace (program : Array Instruction) :
    Nat → Nat → StateOf Value → List Value → Nat → StateOf Value → Prop where
  | done (pc : Nat) (state : StateOf Value) : CleanupCoreTrace program 0 pc state [] pc state
  | advance {cost pc exit : Nat} {state next final : StateOf Value} {pending rest : List Value}
      {instruction : Instruction}
      (action : CleanupCoreAction state pending instruction next rest)
      (code : program[pc]? = some instruction)
      (tail : CleanupCoreTrace program cost (pc + 1) next rest exit final) :
      CleanupCoreTrace program (cost + 1) pc state pending exit final

  | jump {cost pc target exit : Nat} {state final : StateOf Value} {pending : List Value}
      (code : program[pc]? = some (.jump target))
      (tail : CleanupCoreTrace program cost target state pending exit final) :
      CleanupCoreTrace program (cost + 1) pc state pending exit final
  | branch {cost pc condition target exit : Nat} {state final : StateOf Value}
      {pending : List Value} {flag : Bool}
      (code : program[pc]? = some (.branchFalse condition target))
      (value : get state condition = .ok (.boolean flag))
      (tail : CleanupCoreTrace program cost (if flag then pc + 1 else target) state pending exit final) :
      CleanupCoreTrace program (cost + 1) pc state pending exit final

  | constant {cost pc dst exit : Nat} {state final : StateOf Value} {pending : List Value} (value : UInt64)
      (code : program[pc]? = some (.constant dst value))
      (writable : state.registers[dst]? = some none ∨
        ∃ old, state.registers[dst]? = some (some (.number old)))
      (tail : CleanupCoreTrace program cost (pc + 1)
        { state with registers := state.registers.set dst (some (.number value)) } pending exit final) :
      CleanupCoreTrace program (cost + 1) pc state pending exit final

/-- The checked action witnesses derive an actual Core path and exact agreement
    with the complete worklist runner. No executed-step or simulation callback
    appears as a premise. Empty cleanup takes zero instructions. -/
theorem CleanupCoreTrace.correct (program : Array Instruction) (cost pc exit : Nat)
    (state final : StateOf Value) (pending : List Value)
    (trace : CleanupCoreTrace program cost pc state pending exit final) :
    ExecutionPath program cost pc state exit final ∧
      Storage.runCleanup cost state.memory pending = (final.memory, .complete, []) ∧
      final.authority = state.authority := by
  induction trace with
  | done address state => exact ⟨.refl address state, rfl, rfl⟩
  | @advance count address target before next after work rest instruction action code tail ih =>
    obtain ⟨performed, model, authority⟩ := action.correct before next work rest instruction
    refine ⟨.advance (.instruction code performed) ih.1, ?_, ih.2.2.trans authority⟩
    have nonempty : work ≠ [] := by cases action <;> simp
    cases work with
    | nil => exact False.elim (nonempty rfl)
    | cons value work =>
      simpa only [Storage.runCleanup, model] using ih.2.1

  | @jump count address target exit before after work code tail ih =>
    have complete : (Storage.runCleanup count before.memory work).2.1 = .complete := by rw [ih.2.1]
    exact ⟨.advance (.jump code) ih.1,
      (Storage.run_cleanup_complete_succ_fuel count before.memory work complete).trans ih.2.1, ih.2.2⟩
  | @branch count address condition target exit before after work flag code value tail ih =>
    have complete : (Storage.runCleanup count before.memory work).2.1 = .complete := by rw [ih.2.1]
    exact ⟨.advance (.branch code value) ih.1,
      (Storage.run_cleanup_complete_succ_fuel count before.memory work complete).trans ih.2.1, ih.2.2⟩

  | @constant count address dst exit before after work value code writable tail ih =>
    have performed : step before (.constant dst value) =
        .ok { before with registers := before.registers.set dst (some (.number value)) } := by
      rcases writable with empty | ⟨old, present⟩
      · simp only [step, putCopy, empty]
      · simp only [step, putCopy, present]
    have complete : (Storage.runCleanup count before.memory work).2.1 = .complete := by rw [ih.2.1]
    exact ⟨.advance (.instruction code performed) ih.1,
      (Storage.run_cleanup_complete_succ_fuel count before.memory work complete).trans ih.2.1, ih.2.2⟩

/-- The same witness executes under the shared bounded Core interpreter for
    arbitrary continuation fuel, rather than a separate cleanup evaluator. -/
theorem CleanupCoreTrace.continues (program : Array Instruction) (cost pc exit : Nat)
    (state final : StateOf Value) (pending : List Value)
    (trace : CleanupCoreTrace program cost pc state pending exit final) :
    Continues program cost pc state exit final :=
  (trace.correct program cost pc exit state final pending).1.continues program cost pc exit state final

end Witchy.Core

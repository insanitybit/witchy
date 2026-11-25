import Witchy.CleanupOwnerSelectionCore

namespace Witchy.Core

/-- Incoming witnesses for one emitted cleanup instruction. Constructors state
    register/storage facts only, never that Core or cleanup execution succeeds.
    Pending values may be represented in arbitrary distinct compiler registers. -/
inductive CleanupCoreAction : StateOf Value → List Value → Instruction → StateOf Value → List Value → Prop where
  | dispose (s : StateOf Value) (source : Nat) (value : Value) (tail : List Value)
      (present : s.registers[source]? = some (some value))
      (ready : Storage.HeapDisposalReady s.memory value) :
      CleanupCoreAction s (value :: tail) (disposalInstruction source value)
        { s with memory := (Storage.disposeHeapValue s.memory value).1, registers := s.registers.set source none } tail
  | expand (s : StateOf Value) (root dst src index : Nat) (offset : UInt64)
      (block : Storage.Block Value) (child : Value) (tail : List Value)
      (owner : s.registers[src]? = some (some (.owner root)))
      (empty : s.registers[dst]? = some none) (position : number s index = .ok offset)
      (stored : Memory.block? s.memory root = some block)
      (selected : block.lastInitialized = some (offset.toNat, child))
      (free : (Memory.active s.memory root).isEmpty = true)
      (eligible : TransferUnborrowed s.memory child) :
      CleanupCoreAction s (.owner root :: tail) (.takeMove dst src index)
        { s with memory := { s.memory with blocks := s.memory.blocks.set root (some ⟨block.slots.set offset.toNat none⟩) }, registers := s.registers.set dst (some child) }
        (child :: .owner root :: tail)

/-- The primitive bridge is derived from incoming evidence for every action;
    this is not a caller-supplied simulation assumption. -/
theorem CleanupCoreAction.correct (s next : StateOf Value) (pending rest : List Value)
    (instruction : Instruction) (action : CleanupCoreAction s pending instruction next rest) :
    step s instruction = .ok next ∧
      Storage.cleanupWorkStep s.memory pending = (next.memory, .ok (), rest) ∧ next.authority = s.authority := by
  cases action with
  | dispose source value tail present ready =>
    have performed := heap_disposal_effect s source value present ready
    have accepted := Storage.heap_disposal_ready s.memory value ready
    have finished : Storage.finishCleanupValue s.memory value rest =
        ((Storage.disposeHeapValue s.memory value).1, .ok (), rest) := by
      cases outcome : Storage.disposeHeapValue s.memory value with
      | mk memory answer =>
        rw [outcome] at accepted
        cases accepted
        simp only [Storage.finishCleanupValue, outcome]
    refine ⟨performed, ?_, rfl⟩
    cases value with
    | owner root =>
      obtain ⟨block, stored, _free, empty⟩ := ready
      have selected := block.last_initialized_none.mpr empty
      simp only [Storage.cleanupWorkStep, stored, selected, finished]
    | _ => simp only [Storage.cleanupWorkStep, finished]
  | expand root dst src index offset block child tail owner empty position stored selected free eligible =>
    have initialized := block.last_initialized_present offset.toNat child selected
    have permission : Memory.resolve s.memory (.owner root) true = .ok root := by
      simp [Memory.resolve, free]
    have accessed : access s src = .ok (.owner root) := by
      simp [access, get, owner, Bind.bind, Pure.pure]
    have taken : Memory.take s.memory (.owner root) offset.toNat =
        ({ s.memory with blocks := s.memory.blocks.set root (some ⟨block.slots.set offset.toNat none⟩) }, .ok child) := by
      simp [Memory.take, permission, stored, initialized]
    have accepted : (Memory.take s.memory (.owner root) offset.toNat).2 = .ok child := by rw [taken]
    have eligibleAfter : TransferUnborrowed (Memory.take s.memory (.owner root) offset.toNat).1 child := by
      cases child <;> simp_all [TransferUnborrowed, Memory.active]
    have performed := take_move_effect s dst src index offset (.owner root) child empty accessed position accepted eligibleAfter
    refine ⟨by simpa only [taken] using performed, ?_, rfl⟩
    simpa only [Storage.cleanupWorkStep, stored, selected] using
      Storage.expand_cleanup_owner_ready s.memory root offset.toNat block child tail stored free initialized eligible

end Witchy.Core

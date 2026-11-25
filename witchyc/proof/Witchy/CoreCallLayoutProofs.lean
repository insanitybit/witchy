import Witchy.CoreCallLayoutControl
import Witchy.CoreCallBoundaryProofs
import Witchy.CoreCallReturnSelection

namespace Witchy.Core.Call

theorem leave_layout_selection (program : Array Procedure) (machine next : Machine)
    (layout : ReturnLayout) (accepted : leaveLayout program machine layout = .ok next) :
    ∃ selected, layout.Selected machine.store machine.active.base selected := by
  simp only [leaveLayout, Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  exact ⟨_, layout.select_sound machine.store machine.active.base _ (by assumption)⟩

private theorem layout_transfers_frame (callerBase calleeBase index : Nat)
    (destinations results selected : List Nat) (inside : index < calleeBase)
    (outside : ∀ destination ∈ destinations, callerBase + destination ≠ index) :
    ∀ pair ∈ layoutTransfers callerBase calleeBase destinations results selected,
      pair.1 ≠ index ∧ pair.2 ≠ index := by
  intro pair member
  unfold layoutTransfers at member
  obtain ⟨⟨destination, source⟩, endpointMember, same⟩ := List.mem_map.mp member
  subst pair
  have endpoints := List.of_mem_zip (List.mem_filter.mp endpointMember).1
  exact ⟨outside destination endpoints.1, by dsimp only; omega⟩

theorem leave_layout_frame (program : Array Procedure) (machine next : Machine)
    (layout : ReturnLayout) (frame : Continuation) (rest : List Continuation)
    (stack : machine.callers = frame :: rest)
    (accepted : leaveLayout program machine layout = .ok next)
    (index : Nat) (inside : index < machine.active.base)
    (outside : ∀ destination ∈ frame.destinations, frame.caller.base + destination ≠ index) :
    next.store.registers[index]? = machine.store.registers[index]? := by
  simp only [leaveLayout, stack, Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  exact leave_store_frame machine.store _ _ _ (by assumption) index inside
    (layout_transfers_frame _ _ _ _ _ _ inside outside)

theorem leave_layout_preserves (program : Array Procedure) (machine next : Machine)
    (layout : ReturnLayout) (accepted : leaveLayout program machine layout = .ok next)
    (known : StorageStateInvariant machine.store) :
    StorageStateInvariant next.store ∧ CapabilityConserved machine.store next.store ∧
      next.store.memory = machine.store.memory ∧ next.windowValid := by
  simp only [leaveLayout, Bind.bind, Pure.pure] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    obtain ⟨invariant, capabilities, memory, length⟩ :=
      leave_store_preserves machine.store _ _ _ (by assumption) known
    refine ⟨invariant, capabilities, memory, ?_⟩
    dsimp only [Machine.windowValid]
    rw [length]
    simp_all only [ReturnPolicy]

theorem leave_layout_of_ready (program : Array Procedure) (machine : Machine) (procedure : Procedure)
    (frame : Continuation) (rest : List Continuation) (layout : ReturnLayout)
    (selected : List Nat) (store : StateOf Value)
    (found : program[machine.active.procedure]? = some procedure) (stack : machine.callers = frame :: rest)
    (policy : ReturnPolicy machine procedure frame) (signature : layout.slots = procedure.results)
    (vacant : vacantSlots machine.store frame.caller.base frame.destinations = true)
    (selection : layout.select machine.store machine.active.base = .ok selected)
    (ready : leaveStore machine.store machine.active.base
      (layoutTransfers frame.caller.base machine.active.base frame.destinations procedure.results selected) = .ok store) :
    leaveLayout program machine layout = .ok ⟨store, frame.caller, rest⟩ := by
  simp only [leaveLayout, found, Bind.bind, stack, policy, signature, and_self, ↓reduceIte,
    vacant, selection, ready, Pure.pure, List.tail_cons]

end Witchy.Core.Call

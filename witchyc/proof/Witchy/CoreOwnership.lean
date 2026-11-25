import Witchy.ResourceMemory

namespace Witchy.Core

theorem sum_zero_map (items : List α) : (items.map (fun _ => (0 : Nat))).sum = 0 := by
  induction items with
  | nil => rfl
  | cons head tail ih => simpa using ih

theorem initial_owns_resources (limits : Memory.Limits) (roots : List Authority.Token)
    (registers : Nat) (s : State) (accepted : initial limits roots registers = .ok s) :
    OwnsResources s := by
  unfold initial initialOf at accepted
  split at accepted
  · cases accepted
    intro resource
    cases resource <;>
      simp [registerCount, memoryCount, Memory.initialOf, Memory.block?, Memory.loan?,
        List.mapIdx_eq_zipIdx_map, List.map_map, Function.comp_def, weight, valueResource,
        sum_zero_map]
  · cases accepted

theorem putCopy_memory (s next : State) (dst : Nat) (value : Value)
    (accepted : putCopy s dst value = .ok next) : next.memory = s.memory := by
  unfold putCopy at accepted
  repeat' first | (split at accepted) | (cases accepted <;> rfl)

theorem putCopy_preserves_ownership (s next : State) (dst : Nat) (value : Value)
    (owned : OwnsResources s) (accepted : putCopy s dst value = .ok next) : OwnsResources next := by
  intro resource
  rw [putCopy_register_count s next dst value resource accepted,
    putCopy_memory s next dst value accepted]
  exact owned resource

theorem ownership_memory_frame (s : State) (memory : Memory.State)
    (owned : OwnsResources s)
    (same : ∀ resource, memoryCount memory resource = memoryCount s.memory resource) :
    OwnsResources { s with memory } := by
  intro resource
  exact (owned resource).trans (same resource).symm

theorem move_preserves_ownership (s next : State) (src dst : Nat) (value : Value)
    (owned : OwnsResources s) (present : get s src = .ok value)
    (inserted : put s dst value = .ok next) :
    OwnsResources { next with registers := next.registers.set src none } := by
  obtain ⟨empty, shape⟩ := put_shape s next dst value inserted
  subst next
  intro resource
  exact (move_register_count s.registers dst src value resource empty
    (get_register_lookup s src value present)).trans (owned resource)

/-- Register insertion and memory creation must account for the same identity.
    This lemma applies to both allocation and borrowing. -/
theorem put_created_resource (s next : State) (memory : Memory.State)
    (dst : Nat) (value : Value) (owned : OwnsResources s)
    (created : ∀ resource, memoryCount memory resource =
      memoryCount s.memory resource + weight resource (some value))
    (accepted : put { s with memory } dst value = .ok next) : OwnsResources next := by
  intro resource
  have registers := put_register_count { s with memory } next dst value resource accepted
  have shape := (put_shape { s with memory } next dst value accepted).2
  have memoryEqual : next.memory = memory := by rw [shape]
  rw [registers, memoryEqual]
  exact (congrArg (fun n => n + weight resource (some value)) (owned resource)).trans
    (created resource).symm

theorem allocation_put_preserves_ownership (s next : State) (count dst : Nat)
    (handle : Memory.Handle) (owned : OwnsResources s)
    (allocated : (Memory.allocate s.memory count).2 = .ok handle)
    (inserted : put { s with memory := (Memory.allocate s.memory count).1 }
      dst (.owner handle) = .ok next) : OwnsResources next := by
  exact put_created_resource s next _ dst (.owner handle) owned
    (allocation_resource_accounting s.memory count handle allocated) inserted

theorem borrow_put_preserves_ownership (s next : State) (handle : Memory.Handle)
    (exclusive : Bool) (dst : Nat) (identity : Memory.LoanId) (owned : OwnsResources s)
    (valid : Memory.LoansValid s.memory)
    (borrowed : (Memory.borrow s.memory handle exclusive).2 = .ok identity)
    (inserted : put { s with memory := (Memory.borrow s.memory handle exclusive).1 }
      dst (.reference identity) = .ok next) : OwnsResources next := by
  exact put_created_resource s next _ dst (.reference identity) owned
    (borrow_resource_accounting s.memory handle exclusive identity valid borrowed) inserted

/-- Clearing the matching register cancels resource destruction on the memory
    side. This formulation does not assume that positive slot quota was used. -/
theorem clear_destroyed_resource (s : State) (memory : Memory.State) (src : Nat)
    (value : Value) (owned : OwnsResources s) (present : get s src = .ok value)
    (destroyed : ∀ resource, memoryCount memory resource + weight resource (some value) =
      memoryCount s.memory resource) :
    OwnsResources { s with memory, registers := s.registers.set src none } := by
  intro resource
  have equation := register_set_accounting s.registers src (some value) none resource
    (get_register_lookup s src value present)
  have original := owned resource
  have removed := destroyed resource
  have emptyWeight : weight resource none = 0 := by simp [weight]
  rw [emptyWeight, Nat.add_zero] at equation
  change registerCount (s.registers.set src none) resource = memoryCount memory resource
  omega

theorem release_clear_preserves_ownership (s : State) (src : Nat) (handle : Memory.Handle)
    (owned : OwnsResources s) (present : get s src = .ok (.owner handle))
    (released : (Memory.release s.memory handle).2 = .ok ()) :
    OwnsResources { s with
      memory := (Memory.release s.memory handle).1
      registers := s.registers.set src none } := by
  exact clear_destroyed_resource s _ src (.owner handle) owned present
    (release_resource_accounting s.memory handle released)

theorem end_clear_preserves_ownership (s : State) (src : Nat) (identity : Memory.LoanId)
    (owned : OwnsResources s) (present : get s src = .ok (.reference identity))
    (ended : (Memory.endLoan s.memory identity).2 = .ok ()) :
    OwnsResources { s with
      memory := (Memory.endLoan s.memory identity).1
      registers := s.registers.set src none } := by
  exact clear_destroyed_resource s _ src (.reference identity) owned present
    (end_resource_accounting s.memory identity ended)

theorem clear_nonresource_preserves_ownership (s : State) (src : Nat) (value : Value)
    (owned : OwnsResources s) (present : get s src = .ok value)
    (scalar : valueResource value = none) :
    OwnsResources { s with registers := s.registers.set src none } := by
  apply clear_destroyed_resource s s.memory src value owned present
  intro resource
  simp [weight, scalar]

theorem put_capability_preserves_ownership (s next : State) (dst identity : Nat)
    (owned : OwnsResources s) (accepted : put s dst (.capability identity) = .ok next) :
    OwnsResources next := by
  apply put_created_resource s next s.memory dst (.capability identity) owned _ accepted
  intro resource
  simp [weight, valueResource]

end Witchy.Core

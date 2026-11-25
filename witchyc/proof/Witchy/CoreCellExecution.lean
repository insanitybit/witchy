import Witchy.ScalarAggregateFrames
import Witchy.AuthorityProofs

namespace Witchy.Core

variable {Cell : Type} [StorageCell Cell]

/-- The shared run loop lifts local preservation for every storage specialization. -/
theorem run_preserves_cells (predicate : StateOf Cell → Prop)
    (preserve : ∀ s next instruction, predicate s → step s instruction = .ok next → predicate next)
    (program : Array Instruction) (fuel pc : Nat) (s : StateOf Cell) (hs : predicate s) :
    predicate (run program fuel pc s).1 := by
  induction fuel generalizing pc s with
  | zero => exact hs
  | succ fuel ih =>
    cases hp : program[pc]? with
    | none => simpa [run, hp] using hs
    | some instruction =>
      cases instruction <;> simp only [run, hp]
      all_goals first
        | exact ih _ _ hs
        | (split <;> first
          | exact hs
          | exact ih _ _ hs
          | (apply ih; exact preserve _ _ _ hs ‹_›))

omit [StorageCell Cell] in
private theorem put_authority (s next : StateOf Cell) (dst : Nat) (value : Value)
    (accepted : put s dst value = .ok next) : next.authority = s.authority := by
  simp only [put, Bind.bind] at accepted
  split at accepted <;> cases accepted
  rfl
omit [StorageCell Cell] in
private theorem copy_authority (s next : StateOf Cell) (dst : Nat) (value : Value)
    (accepted : putCopy s dst value = .ok next) : next.authority = s.authority := by
  unfold putCopy at accepted
  split at accepted <;> cases accepted
  all_goals rfl

/-- The core cannot mutate authority through ordinary memory/register work.
    Its only authority transitions use the specified Authority operations. -/
theorem step_authority_property_cells (predicate : Authority.State → Prop)
    (s next : StateOf Cell) (instruction : Instruction) (hs : predicate s.authority)
    (preserve : ∀ command, predicate (Authority.step s.authority command).1)
    (h : step s instruction = .ok next) : predicate next.authority := by
  cases instruction
  case packMove dst sources widths =>
    rw [(pack_context s next dst sources widths h).2]
    exact hs
  case unpackMove destinations src widths =>
    rw [(unpack_context s next destinations src widths h).2]
    exact hs
  all_goals simp only [step, Bind.bind, Pure.pure, fromMemory, fromAuthority] at h
  all_goals
    repeat' first | split at h | (cases h)
  all_goals try first | exact hs | exact preserve _
  all_goals grind only [→ put_authority, → copy_authority]

/-- Authority attenuation is independent of the selected cell representation. -/
theorem run_authority_valid_cells (roots : List Authority.Token) (program : Array Instruction)
    (fuel pc : Nat) (s : StateOf Cell) (valid : Authority.Valid roots s.authority) :
    Authority.Valid roots (run program fuel pc s).1.authority := by
  apply run_preserves_cells (fun state => Authority.Valid roots state.authority) _ program fuel pc s valid
  intro state next instruction known accepted
  exact step_authority_property_cells (Authority.Valid roots) state next instruction known
    (fun command => Authority.step_valid roots state.authority command known) accepted

end Witchy.Core

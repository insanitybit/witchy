import Witchy.CoreScalarInvariant
import Witchy.HeapCapabilityNarrow

namespace Witchy.Core

theorem authority_storage_invariant (s : StateOf Value) (authority : Authority.State)
    (known : StorageStateInvariant s) (growth : s.authority.tokens.length ≤ authority.tokens.length) :
    StorageStateInvariant { s with authority } :=
  ⟨known.inventory, known.structural,
    Storage.all_caps_monotone s.authority authority s.registers s.memory growth known.capabilities, known.roots⟩

private theorem capability_put_invariant (s next : StateOf Value) (dst token : Nat)
    (inserted : put s dst (.capability token) = .ok next)
    (known : StorageStateInvariant s) (issued : Storage.valueCapabilitiesValid s.authority (.capability token)) :
    StorageStateInvariant next := by
  have roots := put_loan_roots s next dst (.capability token) known.roots inserted
  obtain ⟨_, shape⟩ := put_shape _ _ _ _ inserted
  have counts := fun resource => put_register_count s next dst (.capability token) resource inserted
  rw [shape] at roots counts ⊢
  refine ⟨?_, known.structural, ⟨?_, known.capabilities.2⟩, roots⟩
  · intro resource
    change registerCount _ resource + _ = _
    rw [counts resource, show weight resource (some (.capability token)) = 0 from rfl, Nat.add_zero]
    exact known.inventory resource
  · exact Storage.cells_caps_set s.authority s.registers dst _ known.capabilities.1
      (by intro value same; cases same; exact issued)

private theorem from_authority_ok (outcome : Authority.Result) (value : Option Nat)
    (accepted : fromAuthority outcome = .ok value) : outcome = .ok value := by
  cases outcome <;> simp_all [fromAuthority]

theorem narrow_preserves_storage (s next : StateOf Value) (dst source : Nat) (rights : Authority.Token)
    (known : StorageStateInvariant s) (accepted : step s (.narrow dst source rights) = .ok next) :
    StorageStateInvariant next := by
  simp only [step, Bind.bind, fromAuthority] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals
    apply capability_put_invariant _ _ _ _ accepted
    · exact authority_storage_invariant s _ known (Storage.authority_tokens_monotone s.authority _)
    · exact Storage.narrowing_created_valid s.authority _ rights _ (from_authority_ok _ _ (by assumption))

theorem emit_preserves_storage (s next : StateOf Value) (source value : Nat)
    (known : StorageStateInvariant s) (accepted : step s (.emit source value) = .ok next) :
    StorageStateInvariant next := by
  simp only [step, Bind.bind, Pure.pure, fromAuthority] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals exact authority_storage_invariant s _ known (Storage.authority_tokens_monotone s.authority _)

theorem emit_byte_preserves_storage (s next : StateOf Value) (source value : Nat)
    (known : StorageStateInvariant s) (accepted : step s (.emitByte source value) = .ok next) :
    StorageStateInvariant next := by
  simp only [step, Bind.bind, Pure.pure, fromAuthority] at accepted
  repeat' first | split at accepted | cases accepted
  all_goals exact authority_storage_invariant s _ known (Storage.authority_tokens_monotone s.authority _)

end Witchy.Core

import Witchy.SourceObligation
import Witchy.AuditRules
namespace Witchy.SourceReplayExamples
-- This named tactic has no ordinary-search fallback: success proves raw terms
-- really reached the current-kernel replay path with extra/reordered premises.
theorem accepted (noise y x : Nat) (_extra : noise = noise) (current_equality : x = y) : x = y := by
  witchy_replay_source_obligation

def marker : Nat := 1
theorem changed_goal : marker = 1 := by
  fail_if_success witchy_replay_source_obligation
  witchy_source_obligation

theorem absent_from_old (x : Nat) : x = x := by
  fail_if_success witchy_replay_source_obligation
  witchy_source_obligation
end Witchy.SourceReplayExamples
#audit_witchy

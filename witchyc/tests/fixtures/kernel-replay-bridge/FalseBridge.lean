import Witchy.SourceObligation
import Witchy.AuditRules
namespace Witchy.SourceReplayExamples
theorem cannot_reuse (self : Nat) : self < self := by
  witchy_source_obligation
end Witchy.SourceReplayExamples
#audit_witchy

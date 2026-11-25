import Witchy.AuditRules
namespace Witchy.SourceReplayExamples
private theorem auxiliary (x y : Nat) (equality : x = y) : x = y := equality
theorem accepted (x y : Nat) (old_equality : x = y) : x = y := auxiliary x y old_equality
def marker : Nat := 0
theorem changed_goal : marker = 0 := rfl
theorem cannot_reuse (self : Nat) : self = self := rfl
end Witchy.SourceReplayExamples
#audit_witchy

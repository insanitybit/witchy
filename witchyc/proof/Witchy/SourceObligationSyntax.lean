import Init

/-! One parser kind shared by exact quoted recipes and the optional engine.
No failure macro or placeholder elaborator can preempt the actual handler. -/
namespace Witchy
syntax (name := sourceObligationReplay) "witchy_replay_source_obligation" : tactic
end Witchy

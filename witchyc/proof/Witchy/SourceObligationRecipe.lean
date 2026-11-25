import Witchy.ScalarPostNormalizer
import Witchy.TypedStorageRuntime
import Witchy.SourceObligationSyntax

/-! Internal source-obligation recipes. Each expansion is the former generated
sequence, in its original order and with its original failure behavior. The
record variant inserts only the two existing record-goal alternatives.

Goal-only simplification and congruence search precede broad context rewriting:
self-read heap equalities must not become recursive rewrite rules early. The
bounded Fin bridge is kept before optional/in-bounds lookup reasoning, and each
`solve` branch is retained only when it closes every goal. Every expansion still
produces an ordinary kernel-checked theorem with the current local context.
-/

macro "witchy_source_obligation" : tactic =>
  `(tactic|
    first
    | witchy_replay_source_obligation
    | assumption
    | rfl
    | (solve | grind only [→ _root_.Witchy.Storage.scalar_cell, _root_.Witchy.Storage.Scalar.bound])
    | (solve | simp)
    | (solve | grind)
    | (solve | simp only [Fin.forall_iff] at *; grind)
    | (solve | (simp_all; (repeat' apply And.intro); all_goals first | omega | (intros; grind only [List.getElem?_set, List.getElem_set, List.length_set, List.getElem?_eq_getElem, Option.bind_some, Option.bind_none, Option.isSome_some, Option.isSome_none])))
    | (solve | (apply _root_.Witchy.Storage.cells_set_cell <;> first | assumption | simp_all))
    | (solve | (apply _root_.Witchy.Storage.cells_option <;> assumption))
    | (solve | simp)
    | omega
    | grind
    | (simp_all; omega)
    | (simp_all; grind)
    | (simp_all; grind [List.eq_replicate_iff, List.mem_iff_getElem])
    | (simp_all; grind [_root_.Witchy.Storage.scalar_cell, _root_.Witchy.Storage.cells_set_none, _root_.Witchy.Storage.cells_set_some])
    | (solve | ((try simp_all); intros; apply _root_.Witchy.Storage.all_initialized; rfl))
    | simp_all
  )

macro "witchy_source_record_obligation" : tactic =>
  `(tactic|
    first
    | witchy_replay_source_obligation
    | assumption
    | rfl
    | (solve | grind only [→ _root_.Witchy.Storage.scalar_cell, _root_.Witchy.Storage.Scalar.bound])
    | (solve | simp)
    | (solve | grind)
    | (solve | simp only [Fin.forall_iff] at *; grind)
    | (solve | (simp_all; (repeat' apply And.intro); all_goals first | omega | (intros; grind only [List.getElem?_set, List.getElem_set, List.length_set, List.getElem?_eq_getElem, Option.bind_some, Option.bind_none, Option.isSome_some, Option.isSome_none])))
    | (solve | (apply _root_.Witchy.Storage.cells_set_cell <;> first | assumption | simp_all))
    | (solve | (apply _root_.Witchy.Storage.cells_option <;> assumption))
    | (solve | simp)
    | (solve | dsimp only [_root_.Witchy.Core.scalarFieldsValid] at *; simp_all)
    | (solve | dsimp only [_root_.Witchy.Storage.CellValid, _root_.Witchy.Storage.ValueValid, _root_.Witchy.Storage.Element.widthsFuel, _root_.Witchy.Storage.Scalar.wellFormed, List.mapM, List.mapM.loop, Bind.bind, Pure.pure] at *; simp_all)
    | omega
    | grind
    | (simp_all; omega)
    | (simp_all; grind)
    | (simp_all; grind [List.eq_replicate_iff, List.mem_iff_getElem])
    | (simp_all; grind [_root_.Witchy.Storage.scalar_cell, _root_.Witchy.Storage.cells_set_none, _root_.Witchy.Storage.cells_set_some])
    | (solve | ((try simp_all); intros; apply _root_.Witchy.Storage.all_initialized; rfl))
    | simp_all
  )

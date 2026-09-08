# Shared terminal-tail entry with multiple loop predecessors

Status: static review hypothesis; executable regression not yet established.

The queens repair in `f00792f9a` restores predecessor checks for terminal
sinks, but `is_exclusive_terminal_loop_tail` still permits predecessors from
any loop member. Both the structured emitter and plan mirror use this rule.

Potential counterexample:

```text
H (loop header) -> A or B
A -> T or H
B -> T or H
T -> Return
```

For tail entry T, its reachable set is only T. Both incoming predecessors
are loop members, so the predicate classifies it as exclusive despite two
distinct exit paths. The suspected consequence is duplicate block emission
and fail-closed refusal, not an demonstrated safety bypass.

Before changing the rule, add a focused canonical CFG reproducer. An
entry-aware condition should distinguish the ordinary single loop-to-tail
edge from multiple incoming paths. Rejecting all predecessors outside the
tail would also reject every ordinary tail entry and could restore the
duration regression. Preserve both duration and full queens execution as
positive evidence. Do not weaken the encoder's single-emission check.

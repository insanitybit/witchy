# Examples

Every file here is a runnable program; from the repo root:

```sh
witchy examples/hello.witchy          # run (interpreter)
witchy check examples/hello.witchy    # type-check (capabilities included)
witchy caps examples/hello.witchy     # show its capability footprint
witchy sandbox examples/hello.witchy  # run confined in the WASM VM
```

## Start here

| Example | Shows |
|---|---|
| [hello](hello.witchy) | Functions, pattern matching, method-call sugar, the Console capability |
| [fizzbuzz](fizzbuzz.witchy), [loops](loops.witchy), [ranges](ranges.witchy) | Control flow, `for`/`while`, ranges |
| [strings](strings.witchy), [tuples](tuples.witchy), [records](records.witchy), [list_ops](list_ops.witchy) | The core data types |
| [patterns](patterns.witchy), [listmatch](listmatch.witchy), [guard](guard.witchy), [let_patterns](let_patterns.witchy) | `match`: ADTs, list patterns, guards, exhaustiveness |
| [result](result.witchy), [try](try.witchy), [option_std](option_std.witchy) | `Option`/`Result` and the `?` operator |
| [closures](closures.witchy), [higher_order](higher_order.witchy), [pipeline](pipeline.witchy) | First-class functions |
| [generics](generics.witchy), [generic_stack](generic_stack.witchy), [traits](traits.witchy), [shapes](shapes.witchy) | Generics, traits, `where` bounds |
| [ownership](ownership.witchy), [conventions](conventions.witchy), [mutate](mutate.witchy) | `let`/`var`/`own` parameter conventions |
| [durations](durations.witchy), [time_and_encoding](time_and_encoding.witchy) | Duration literals, time, hex/base64 |

## The capability system

| Example | Shows |
|---|---|
| [capability_rights](capability_rights.witchy) | `Dir[Read]` vs `Dir[Write]`, `Net[Connect]` vs `Net[Listen]`, narrowing with `as` |
| [branded_caps](branded_caps.witchy) | Wrapping a capability in your own type; the footprint sees through it |
| [files](files.witchy) | Filesystem access through a confined `Dir` |
| [caps_audit](caps_audit.witchy) | A witchy program auditing another's footprint (`compiler.footprint`) |
| [caps_guard](caps_guard.witchy) | A CI gate that exits non-zero on capability widening |
| [coven_check](coven_check.witchy) | Auditing a package's footprint before depending on it |
| [plugin_host](plugin_host.witchy) | Hosting untrusted plugin code with attenuated authority |
| [minigrep](minigrep.witchy) | argv + Env + Dir together |
| [serve_hello](serve_hello.witchy), [serve_api](serve_api.witchy), [serve_static](serve_static.witchy) | `Net[Listen]` HTTP servers (routing, middleware, static files) |

## Algorithms & programs

| Example | Shows |
|---|---|
| [dijkstra](dijkstra.witchy), [toposort](toposort.witchy), [maze](maze.witchy), [bst](bst.witchy) | Graphs and trees |
| [queens](queens.witchy), [sudoku](sudoku.witchy), [life](life.witchy), [pascal](pascal.witchy) | Backtracking, grids |
| [brainfuck](brainfuck.witchy), [calc](calc.witchy), [rpn](rpn.witchy), [eval](eval.witchy) | Interpreters and parsers written in witchy |
| [rle](rle.witchy), [roman](roman.witchy), [anagram](anagram.witchy), [wordcount](wordcount.witchy), [dedup](dedup.witchy) | Text processing |
| [jq](jq.witchy), [parse_kv](parse_kv.witchy), [config_merge](config_merge.witchy), [diff](diff.witchy) | Structured data tools |
| [stats](stats.witchy), [matrix](matrix.witchy), [temperature](temperature.witchy), [floats](floats.witchy) | Numerics |
| [actors](actors.witchy), [counter](counter.witchy), [mailbox](mailbox.witchy), [dispatch](dispatch.witchy) | Actors and message passing |
| [actor_caps](actor_caps.witchy) | Capability-holding actors: per-actor gated VMs, authority granted at `spawn` |
| [lazy_fib](lazy_fib.witchy), [generators](generators.witchy) | Lazy iterators |
| [regions](regions.witchy) | User-controlled allocation scopes (`region:`) |

## Multi-package projects ([projects/](projects/))

Each is a workspace of several runes (witchy packages) with path dependencies,
lockfiles, and capability manifests — built and run through the package manager
(`witchy build` / `witchy run` inside the project directory):

- **todo** — a todo CLI over a persistence rune
- **ledger** — double-entry bookkeeping with a shared core
- **dashboard** — a diamond dependency graph (two runes sharing a base)
- **report**, **sales**, **convert**, **config**, **wordfreq** — data pipelines
  exercising Dir/CSV/JSON across rune boundaries

The package manager itself is also written in witchy: see
[`projects/pm`](../projects/pm) (the client) and
[`projects/coven`](../projects/coven) (the registry server).

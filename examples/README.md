# Examples

Each example is its own **rune** (a witchy package): `<name>/witchy.toml` plus
`<name>/src/<name>.witchy`, a `README.md`, and — where there's logic worth
testing — `src/<name>_test.witchy`. From the repo root:

```sh
witchy examples/hello/src/hello.witchy          # run
witchy check   examples/hello/src/hello.witchy  # type-check (capabilities included)
witchy caps    examples/hello/src/hello.witchy  # show its capability footprint
witchy sandbox examples/hello/src/hello.witchy  # run confined in the WASM VM
witchy test    examples/hello                    # run the rune's tests (if any)
```

Or from inside a rune: `cd examples/hello && witchy run`.

## Start here

| Example | Shows |
|---|---|
| [hello](hello/) | Functions, pattern matching, method-call sugar, the Console capability |
| [fizzbuzz](fizzbuzz/), [loops](loops/), [ranges](ranges/) | Control flow, `for`/`while`, ranges |
| [strings](strings/), [tuples](tuples/), [records](records/), [list_ops](list_ops/) | The core data types |
| [patterns](patterns/), [listmatch](listmatch/), [guard](guard/), [let_patterns](let_patterns/) | `match`: ADTs, list patterns, guards, exhaustiveness |
| [result](result/), [try](try/), [option_std](option_std/) | `Option`/`Result` and the `?` operator |
| [closures](closures/), [higher_order](higher_order/), [pipeline](pipeline/) | First-class functions |
| [generics](generics/), [generic_stack](generic_stack/), [traits](traits/), [shapes](shapes/) | Generics, traits, `where` bounds |
| [ownership](ownership/), [conventions](conventions/), [mutate](mutate/) | `let`/`var`/`own` parameter conventions |
| [durations](durations/), [time_and_encoding](time_and_encoding/) | Duration literals, time, hex/base64 |

## The capability system

| Example | Shows |
|---|---|
| [capability_rights](capability_rights/) | `Dir[Read]` vs `Dir[Write]`, `Net[Connect]` vs `Net[Listen]`, narrowing with `as` |
| [branded_caps](branded_caps/) | Wrapping a capability in your own type; the footprint sees through it |
| [carried_state](carried_state/) | A sealed `capability` record carrying a host cap + policy (audits as the cap, enforces the policy) |
| [files](files/) | Filesystem access through a confined `Dir` |
| [file_capability](file_capability/) | `File[Read]`/`File[Write]` — authority to one file; `dir.read_file`/`dir.write_file` navigate to the leaf |
| [caps_audit](caps_audit/) | A witchy program auditing another's footprint (`compiler.footprint`) |
| [caps_guard](caps_guard/) | A CI gate that exits non-zero on capability widening |
| [coven_check](coven_check/) | Auditing a package's footprint before depending on it |
| [plugin_host](plugin_host/) | Hosting untrusted plugin code with attenuated authority |
| [minigrep](minigrep/) | argv + Env + Dir together |
| [serve_hello](serve_hello/), [serve_api](serve_api/), [serve_static](serve_static/) | `Net[Listen]` HTTP servers (routing, middleware, static files) |

## Performance modes

| Example | Shows |
|---|---|
| [opt_mode](opt_mode/) | `mode opt`: explicit ownership, checked in-place accumulation, and an allocation-free constant-stack functional state kernel |
| [projects/opt_pipeline](projects/opt_pipeline/) | `mode opt` is **transitive** — an `opt` module may only import other `opt` modules (std is exempt), so the discipline covers the whole program |

## Algorithms & programs

| Example | Shows |
|---|---|
| [dijkstra](dijkstra/), [toposort](toposort/), [maze](maze/), [bst](bst/) | Graphs and trees |
| [queens](queens/), [sudoku](sudoku/), [life](life/), [pascal](pascal/) | Backtracking, grids |
| [brainfuck](brainfuck/), [calc](calc/), [rpn](rpn/), [eval](eval/) | Interpreters and parsers written in witchy |
| [rle](rle/), [roman](roman/), [anagram](anagram/), [wordcount](wordcount/), [dedup](dedup/) | Text processing |
| [jq](jq/), [parse_kv](parse_kv/), [config_merge](config_merge/), [diff](diff/) | Structured data tools |
| [stats](stats/), [matrix](matrix/), [temperature](temperature/), [floats](floats/) | Numerics |
| [lazy_fib](lazy_fib/), [generators](generators/) | Lazy iterators |
| [regions](regions/) | User-controlled allocation scopes (`region:`) |

## Concurrency

`async`/`await`, `spawn`, and first-class channels — the Go/CSP model, on a
cooperative executor written in pure witchy (`std/task`, `std/chan`).

| Example | Shows |
|---|---|
| [async_tasks](async_tasks/), [channels](channels/), [for_await](for_await/) | `async fn`/`await`, spawning tasks, sending/receiving over channels |
| [worker_pool](worker_pool/), [select](select/), [request_reply](request_reply/) | A worker pool (mpmc), `select` over channels, request/reply |

## Multi-package projects ([projects/](projects/))

Each is a workspace of several runes (witchy packages) with path dependencies,
lockfiles, and capability manifests — built and run through the package manager
(`witchy build` / `witchy run` inside the project directory):

- **todo** — a todo CLI over a persistence rune
- **ledger** — double-entry bookkeeping with a shared core
- **dashboard** — a diamond dependency graph (two runes sharing a base)
- **report**, **config**, **wordfreq** — data pipelines exercising Dir/JSON/text
  processing across rune boundaries

The package manager itself is also written in witchy: see
[`projects/pm`](../projects/pm) (the client) and
[`projects/coven`](../projects/coven) (the registry server).

## Complete rune inventory

The sections above are a guided tour. This table is the exhaustive index of
top-level runnable example runes; the repository test suite keeps it in sync
with `examples/*/witchy.toml`.

<!-- runnable-inventory:start -->
| Example | Example | Example | Example |
|---|---|---|---|
| [aliases](aliases/) | [anagram](anagram/) | [app](app/) | [async_tasks](async_tasks/) |
| [bank](bank/) | [brainfuck](brainfuck/) | [branded_caps](branded_caps/) | [bst](bst/) |
| [calc](calc/) | [calculator](calculator/) | [capability_rights](capability_rights/) | [caps_audit](caps_audit/) |
| [caps_guard](caps_guard/) | [carried_state](carried_state/) | [channels](channels/) | [closures](closures/) |
| [commands](commands/) | [compute](compute/) | [config_merge](config_merge/) | [constants](constants/) |
| [conventions](conventions/) | [coven_check](coven_check/) | [dedup](dedup/) | [dice](dice/) |
| [diff](diff/) | [dijkstra](dijkstra/) | [display](display/) | [durations](durations/) |
| [equality](equality/) | [eval](eval/) | [file_capability](file_capability/) | [files](files/) |
| [fizzbuzz](fizzbuzz/) | [floats](floats/) | [for_await](for_await/) | [generators](generators/) |
| [generic_stack](generic_stack/) | [generics](generics/) | [guard](guard/) | [hello](hello/) |
| [higher_order](higher_order/) | [higher_order_sum](higher_order_sum/) | [inventory](inventory/) | [jq](jq/) |
| [largest](largest/) | [lazy_fib](lazy_fib/) | [let_patterns](let_patterns/) | [life](life/) |
| [list_more](list_more/) | [list_ops](list_ops/) | [list_pipeline](list_pipeline/) | [listmatch](listmatch/) |
| [loops](loops/) | [math_demo](math_demo/) | [matrix](matrix/) | [maze](maze/) |
| [minigrep](minigrep/) | [mutate](mutate/) | [opt_mode](opt_mode/) | [option_std](option_std/) |
| [ownership](ownership/) | [parse_kv](parse_kv/) | [pascal](pascal/) | [patterns](patterns/) |
| [pipeline](pipeline/) | [plugin_host](plugin_host/) | [predicates](predicates/) | [queens](queens/) |
| [ranges](ranges/) | [record_compiled](record_compiled/) | [record_update](record_update/) | [records](records/) |
| [redis_capability](redis_capability/) | [regions](regions/) | [request_reply](request_reply/) | [rle](rle/) |
| [roman](roman/) | [rpn](rpn/) | [scope](scope/) | [select](select/) |
| [serve_api](serve_api/) | [serve_hello](serve_hello/) | [serve_static](serve_static/) | [shapes](shapes/) |
| [signs](signs/) | [sort](sort/) | [stats](stats/) | [std_demo](std_demo/) |
| [strings](strings/) | [subscript](subscript/) | [sudoku](sudoku/) | [temperature](temperature/) |
| [text](text/) | [time_and_encoding](time_and_encoding/) | [toposort](toposort/) | [traits](traits/) |
| [try](try/) | [tuples](tuples/) | [wordcount](wordcount/) | [worker_pool](worker_pool/) |
| [wrap](wrap/) | [zip](zip/) | | |
<!-- runnable-inventory:end -->

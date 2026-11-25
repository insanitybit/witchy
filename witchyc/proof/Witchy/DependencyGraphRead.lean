import Lean.Environment

namespace Witchy.DependencyClosure.GraphRead

open Lean

/-- Current independent module reads, deduplicated before launch. At most four
tasks exist at once; a failed batch is completely joined before throwing. -/
def collect (read : Name → IO (Array Import)) (imports : Array Import)
    (priority : Task.Priority := Task.Priority.default) :
    IO (NameMap (Array Import)) := do
  let mut seen : NameSet := {}
  let mut frontier : Array Name := #[]
  for imp in imports do
    unless seen.contains imp.module do
      seen := seen.insert imp.module
      frontier := frontier.push imp.module
  let mut graph : NameMap (Array Import) := {}
  while !frontier.isEmpty do
    let mut next : Array Name := #[]
    let mut offset := 0
    while offset < frontier.size do
      let batch := frontier.extract offset (offset + 4)
      let mut tasks : Array (Task (Except IO.Error (Array Import))) := #[]
      for name in batch do
        tasks := tasks.push (← IO.asTask (read name) priority)
      let mut results : Array (Array Import) := #[]
      let mut failure : Option IO.Error := none
      for task in tasks do
        match ← IO.wait task with
        | .ok children => results := results.push children
        | .error error => if failure.isNone then failure := some error
      -- Never short-circuit the joins, including after an earlier read failed.
      if let some error := failure then throw error
      for (name, children) in batch.zip results do
        graph := graph.insert name children
        for imp in children do
          unless seen.contains imp.module do
            seen := seen.insert imp.module
            next := next.push imp.module
      offset := offset + 4
    frontier := next
  return graph

private partial def visit (graph : NameMap (Array Import)) (imports : Array Import) :
    StateRefT (NameSet × Array Name) IO Unit := do
  for imp in imports do
    if (← get).1.contains imp.module then continue
    modify fun (seen, modules) => (seen.insert imp.module, modules)
    let some children := graph.find? imp.module
      | throw <| IO.userError s!"incomplete current dependency graph: {imp.module}"
    visit graph children
    modify fun (seen, modules) => (seen, modules.push imp.module)

/-- The old deterministic depth-first module-after-import order, using only
THIS query's completed reads. No more object reads or environment construction. -/
def ordered (graph : NameMap (Array Import)) (imports : Array Import) : IO (Array Name) := do
  let ((), (_, modules)) ← (visit graph imports).run ({}, #[])
  return modules

end Witchy.DependencyClosure.GraphRead

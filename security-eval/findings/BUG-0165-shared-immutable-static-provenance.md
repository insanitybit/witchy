# BUG-0165: Shared results rejected immutable static storage

## Reproducer

```
fn answer() -> String: "ok"
fn main():
  let f = answer
  let result = f()
  1
```

The source checker assigns the call result the `Shared` ownership contract, but
the memory checker previously required the concrete `ReferenceCounted`
allocation class. The literal result is `StaticData`, so initial validation
failed with `InvalidAllocationClass`.

## Resolution

`Shared` now carries an explicit immutable storage union: concrete RC, exact
static-data provenance, or an RC-or-static boundary value. The union permits
only clone and drop operations; it cannot authorize reclaim, uniqueness,
in-place mutation, or writes. Runtime static clone/drop remain no-ops, while
dynamic references use the RC header path.

## Boundary

`Bump`, `Scratch`, and unproven classless values are rejected even if a caller
attempts to mark them shared immutable.

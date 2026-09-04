# Bug: CFG lowering drops allocation classes on block parameters

## Symptom

A validated reference-counted object transferred through a branch into a loop
or exit block parameter was rejected by the canonical backend at `Reclaim` with
`UnsupportedLayout { operation: "Reclaim" }`.

## Cause

`lower_function` tracked allocation classes only by the value IDs created by
`Alloc`, `MoveValue`, and `Share`.  Branch arguments create new block-parameter
definitions, but lowering did not propagate the class across those edges.  The
validator correctly accepted the object and its reclaim authority; the backend
then failed to recognize the transferred object as reference-counted.

## Fix

Seed allocation classes from operations and propagate them across branch and
branch-if arguments to block parameters to a CFG fixed point before mechanical
lowering.  This changes no validation rule and does not grant reclaim
authority; it only preserves the already-validated allocation class needed by
the backend.

## Regression

`canonical_wasmir::tests::admitted_int_list_loop_executes_with_bounds_checks`
constructs a validated `List(Int)` object, carries it through a loop and exit
join, reclaims it, and executes the admitted Wasm result.

# `using`

```
fn key(allocator: &Allocator, word: &String) -> Result<String, U64>:
    using var letters: Vec<U64> = Vec.new(allocator, word.byte_len())?:
        for index in 0..word.byte_len():
            match letters.push_mut(allocator, word.byte_or_end(index)):
                Ok(done):
                    continue
                Err(back):
                    return Err(1)
        sort(&mut letters)
        using var made = String.new(allocator)?:
            for letter in &letters:
                made.push(allocator, *letter)?
            return Ok(move made)
```

`using name = value:` (or `using var name = value:`) binds an owner for the
indented block and discards it on every way out of the block: falling off its
end, `return`, a `?` that hands back an error, `break` or `continue` of a loop
outside the block, and the value a `let x = match` takes when the block ends a
path. Before, each of those paths closed each owner by hand; `key` above was
23 lines with three closes and is 13 with none
(`tests/fixtures/programs/anagrams.wy`).

This is Witchy 1's structured resource (rfcs/0167-easy-and-optimized-design.md,
section 8): disposal is asked for, not an invisible destructor, and every exit
performs exactly one disposition. The disposer is `Discard.discard`, which
`String`, `Vec<T>`, `HashMap`, `Heap` and the JSON tree implement.

## Handing the owner on

Each exit disposes of the owner exactly once. Where every path to an exit has
moved it away (into a container, to a function, or in the exit itself, as
`return Ok(move made)`), that exit discards nothing
(`tests/fixtures/using/handed-on.wy`). Where some paths have moved it and others
have not, the program is refused: it would be discarded on some paths and not
others (`some-paths.refused.wy`). `programs/diff.wy` reads lines into a
`using` String and moves each into a Vec before `continue`.

## How

`src/compiler/using.rs` rewrites the block before it is checked: a scope holding
`let name = value`, the body with a `Dispose` before each exit, and one at the
end. The checker resolves each `Dispose` from what it knows of the owner at that
point: still held on every path, `name.discard()`; moved on every path,
nothing; otherwise a refusal. `for` and `?` inside are written out first, so the early return a `?`
makes is one of the exits. A `return` whose value reads the owner is worked out
into a hidden name first, then the owner is discarded, then the name is
returned. A value that leaves to a `let x = match` outside and reads the owner
is refused; bind it inside the block first (`leaves-reading.refused.wy`).

The rewrite only adds `Dispose` statements; the borrow and ownership checker
checks the result like any other code, so a type without `Discard`
(`no-discard.refused.wy`) or a loan still in use is refused there. The one new
checker rule is how a `Dispose` is resolved, which reads the same ownership
state every other rule reads; it is tested, not proved. A mistake there could
leave an owner undiscarded (a leak, which the end-of-scope check also catches)
or refuse a safe program; it cannot discard a value twice, since the discard
is checked as an ordinary move.

Fixtures: `tests/fixtures/using/` (`paths.wy`, `chosen.wy`, `handed-on.wy`
run; four refusals), `programs/diff.wy`.
Unit tests in `using.rs` fix where the discards go.

## Not yet

- No `using` without a name, and no disposer other than `Discard`.

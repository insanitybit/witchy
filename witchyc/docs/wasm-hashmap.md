# HashMap, and the checked cell operations it is written with

`library/wasm-hashmap.wy` is a generic `HashMap<K: Any + Hash + Eq, V: Any>`
written as ordinary Witchy code. It has no proofs and uses no unchecked
operation, so nothing in it is trusted beyond the compiler itself.

```
let made: Result<HashMap<String, U64>, U64> = HashMap.new(&allocator, 4)
...
map.insert(move key, 3)          // Result<(), Entry<K, V>>: a present key or a full table hands the entry back
map.get(&key)                    // Result<&V, U64>: the map stays lent while the reference is used
map.contains(&key)
map.remove(&key)                 // Result<Entry<K, V>, U64>
map.take_next(&mut cursor)       // the next entry; in a loop, empties the map in one pass
map.take_any()                   // any one entry (scans from the first cell: emptying a map
                                 // with it is quadratic, 1.5 s for 20,000 entries against 1.2 ms)
map.resize(&allocator, 16)       // Result<HashMap, HashMap>: a new table, every entry moved over
map.try_release() / map.release()
```

The map is sealed. Construct it with `HashMap.new` or `HashMap.for_entries`.
Earlier code that built the record directly or wrote its fields must use the map
methods so entries and fingerprints stay in sync. Shared reads remain available.

The table stores entries in a `Buffer<Entry<K, V>>` with linear probing. The
buffer's occupancy bitmap records which slots hold entries. At an indexed group boundary, lookup uses a 16-lane byte comparison to
skip occupied slots with different fingerprints. Stopping at the first vacancy preserves the insertion slot
returned by a miss. A matching signature still requires an equality check.

For capacities above 16, construction tries to reserve one additional buffer.
Failure to reserve it leaves the map usable with scalar probes. Its payload is
one 16-byte vector for each group of up to 16 slots, or `16 * ceil(capacity / 16)` bytes.
For example, the 666,667-slot integer benchmark reserves 666,672 payload bytes.
The ARM64 allocator requests 671,921 bytes for that buffer, including its
40-byte header and 5,209-byte occupancy bitmap. The system allocator can
round this request up. Both backends also consume one allocation slot.

In the nested-map fixture, an entry containing a U64 key and a map value
occupies 40 bytes on Wasm and 48 on macOS ARM64, up from 24 on both at
`c662a6a9`. This descriptor overhead applies even to small maps that reserve
no group buffer.
Reservation happens at construction, even if the workload never activates the filter.

A long insertion probe can activate the filter if its key and the resident at
its home have different signatures. Groups acquire metadata on subsequent
writes. Unindexed groups are probed normally. An indexed group stores one byte per slot. Zero marks a vacancy and
values 1 through 128 encode seven hash bits. Moving an entry updates its byte
at both the source and destination.

Deletion repairs the probe chain without tombstones. During a long repair, the
last entry in the run can fill the hole if that entry's probe path crosses it.
Otherwise repair continues with selective backward shifts. `insert` keeps the
current capacity. `put` attempts growth past three-quarters load, and
`resize` changes capacity on request.

`library/wasm-hash.wy` declares the two traits a key needs, `Hash` and `Eq`,
and implements them for `U64`. `library/wasm-string.wy` implements them for
`String`. `tests/fixtures/hashmap/numbers.wy` drives a map whose keys all
collide; `tests/fixtures/hashmap/words.wy` is a `HashMap<String, U64>`.

## Grouped lookups

For a read-heavy phase with long probe chains, import `grouped_hashmap` and call
`map.into_grouped(&allocator)`. It consumes the map and samples evenly spaced slots. When a
sampled key is at least 16 slots from home, it scans the table to build a
fingerprint index. Otherwise lookups use the original table directly. A sample
can miss a local cluster, which affects performance only. Allocation failure
returns the original map in `Err`.
The fingerprint index stores 16 fingerprint bytes per group of 16 slots, including padding
in the last group. Its `get(&query)` compares those bytes with Witchy's
`U8x16`, checks key equality for candidate slots, and stops at the first empty
slot. It accepts the same borrowed keys and returns the same miss index as
`HashMap.get`. Call `index.into_map()` to release the index and resume writes.

## Checked cell operations

Three general additions made this possible. None knows about hash maps.

- `memory.try_init(&mut buffer, index, item) -> Result<(), T>` fills an empty
  cell. When the index is out of range or the cell is already full, the item
  comes back in `Err`, so an owner is never dropped. (`memory.init` returns an
  error code and is limited to Copy elements for that reason.)
- `memory.ref(&buffer, index) -> Result<&T, U64>` is a reference to an occupied
  cell, or code 1 (out of range) or 2 (empty). The buffer stays lent while the
  reference is used.
- `&place` is a value: `&(*entry).value`, `&shelf.count`. It lends the place's
  root; when the place is reached through a reference, it borrows whatever that
  reference borrows. The ordinary rules for references apply
  (`docs/wasm-references.md`).

Both cell operations go through the same runtime `slot_prepare` check as
`memory.init` and `memory.take`: bounds and occupancy are tested on every call.
That costs a branch per access; a container that wants the branch gone states
an invariant and uses the `_unchecked` forms with a proof, as `Vec` does.

## Not yet

- Nothing checks that keys which are equal (`Eq`) hash the same; a wrong `Hash`
  impl loses lookups but cannot break memory safety.
- Entries are walked with `for entry in &map:` (through `next`), in the order of the
  cells, not of insertion; `entry(slot)` reads one cell. `get_mut` changes a value in
  place.
- `release` traps on a map that still holds entries; `discard()` empties and releases it.
- A proved function still cannot call these operations or a trait method.

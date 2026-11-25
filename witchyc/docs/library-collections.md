# Collections: library reference

Generated from the library source by `scripts/library-reference.py`; each line is a public
function as declared, with the first sentence of its comment. `self: &String` is called as
`text.name(...)`; a function with no `self` is called as `name(...)`.

## `import vec` (library/wasm-vec.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `Vec<T>.new` | `(allocator: &Allocator, requested: U64) -> Result<Vec<T>, U64>` | An empty Vec with room for `requested` elements. |
| `Vec<T>.len` | `(value: &Vec<T>) -> U64` | How many elements it holds. |
| `Vec<T>.capacity` | `(value: &Vec<T>) -> U64` | How many it has room for before it must grow. |
| `Vec<T>.get` | `(value: &Vec<T>, index: U64) -> Result<T, U64>` | A copy of the element at `index`. |
| `Vec<T>.at` | `(value: &Vec<T>, index: U64) -> Result<&T, U64>` | A reference to an element in place. |
| `Vec<T>.at_mut` | `(value: &mut Vec<T>, index: U64) -> Result<&mut T, U64>` | A reference to an element for changing it where it is. |
| `Vec<U8>.push_word_mut` | `(vector: &mut Vec<U8>, word: U64, count: U64) -> Result<(), U64>` | Append the low `count` bytes of `word`, lowest first: eight bytes in one step where one at a time would be eight (`String.push_text` copies a literal a word at a time). |
| `Vec<U8>.push_from_mut` | `(vector: &mut Vec<U8>, source: &Vec<U8>, at: U64, count: U64) -> Result<(), U64>` | Append the `count` bytes of `source` from `at`, at most eight, in one read and one write (`String.push_string` copies a word at a time). |
| `Vec<U8>.word_at` | `(vector: &Vec<U8>, from: U64, count: U64) -> U64` | Bytes `from..from + count` (eight when `count` is more) as one number, lowest byte first: a byte past `count` or past the end reads as zero. |
| `Vec<T>.close` | `(input: Vec<T>) -> ()` | The cells past `length` are empty already, so only the first `length` are emptied. |
| `Vec<T>.try_release` | `(input: Vec<T>) -> Result<(), Vec<T>>` | Release an empty Vec. |
| `Vec<T>.push_spare` | `(input: Vec<T>, item: T) -> Result<Vec<T>, VecPushError<T>>` | Append into room already there, by value. |
| `Vec<T>.take_last` | `(input: Vec<T>) -> Result<VecTaken<T>, Vec<T>>` | Take the last element out, by value. |
| `Vec<T>.reserve` | `(input: Vec<T>, allocator: &Allocator, requested: U64) -> Result<Vec<T>, Vec<T>>` | Make room for `requested` elements in all, by value. |
| `Vec<T>.push` | `(input: Vec<T>, allocator: &Allocator, item: T) -> Result<Vec<T>, VecPushError<T>>` | Append, growing when full, by value. |
| `Vec<T>.push_spare_mut` | `(vector: &mut Vec<T>, item: T) -> Result<(), T>` | Append into room already there. |
| `Vec<T>.take_last_mut` | `(vector: &mut Vec<T>) -> Result<T, U64>` | Move the last element out. |
| `Vec<T>.replace_mut` | `(vector: &mut Vec<T>, index: U64, item: T) -> Result<T, T>` | Put `item` where the element at `index` was and hand that element back. |
| `Vec<T>.truncate` | `(input: Vec<T>, length: U64) -> Vec<T>` | Drop trailing items until at most `length` remain. |
| `Vec<T>.filled` | `(allocator: &Allocator, count: U64, item: T) -> Result<Vec<T>, U64>` | A vector of `count` copies of `item`. |
| `Vec<T>.index` | `(vector: &Vec<T>, index: U64) -> T` | `items[index]`: the element at `index`. |
| `Vec<T>.index_set` | `(vector: &mut Vec<T>, index: U64, item: T) -> ()` | `items[index] = item`. |
| `Vec<T>.set` | `(vector: &mut Vec<T>, index: U64, item: T) -> Result<(), U64>` | Store a value over the element at `index`. |
| `Vec<T>.swap` | `(vector: &mut Vec<T>, first: U64, second: U64) -> Result<(), U64>` | Exchange the elements at two indexes. |
| `Vec<T>.truncate_mut` | `(vector: &mut Vec<T>, length: U64) -> ()` | Drop trailing items in place until at most `length` remain. |
| `Vec<T>.release_empty` | `(input: Vec<T>) -> ()` | Release a vector that holds no elements. |
| `Vec<T>.trade` | `(vector: &mut Vec<T>, index: U64, held: &mut T) -> ()` | Put `held` where the element at `index` is, and hold that element instead. |
| `Vec<T>.swap_through` | `(vector: &mut Vec<T>, hand: &mut T, first: U64, second: U64) -> ()` | Exchange the elements at two indexes through `hand`, a spare value that ends as it began: three trades in place, so no element leaves the vector (unlike `exchange`, which takes the last one out and puts it back, two calls into the allocator). |
| `Vec<T>.put_back` | `(vector: &mut Vec<T>, item: T) -> ()` | Push into room that is known to be there. |
| `Vec<T>.exchange` | `(vector: &mut Vec<T>, first: U64, second: U64) -> Result<(), U64>` | Exchange the elements at two indexes, owners included. |
| `Vec<T>.reserve_mut` | `(vector: &mut Vec<T>, allocator: &Allocator, requested: U64) -> Result<(), U64>` | Make room for at least `requested` elements. |
| `Vec<T>.push_mut` | `(vector: &mut Vec<T>, allocator: &Allocator, item: T) -> Result<(), T>` | Append, doubling the capacity when the vector is full. |
| `Vec<T>.cell` | `(vector: &Vec<T>, index: U64) -> &T` | `&v[i]`: a reference to the element at `index`; past the end the run stops, as `v[i]`. |
| `Vec<T>.cell_mut` | `(vector: &mut Vec<T>, index: U64) -> &mut T` | `&mut v[i]`: the same, to change the element where it is. |
| `Vec<T>.as_slice` | `(value: &Vec<T>) -> &[T]` | The elements as a slice (docs/design-slices.md): every cell below the length holds a value, which the type's invariant says and the proof uses. |
| `Vec<T>.elements` | `(value: &Vec<T>) -> &[T]` | The elements in order, as one slice: what `for item in &v` walks (iteration.rs: a type with `elements` is walked over it, the length read once and no test per element). |
| `Vec<T>.elements_mut` | `(value: &mut Vec<T>) -> &mut [T]` | The elements in order, as one unique slice: what `for item in &mut v` walks. |
| `Vec<T>.as_mut_slice` | `(value: &mut Vec<T>) -> &mut [T]` | The elements as a `&mut [T]`: they may be written, and the vector stays lent uniquely while the slice is used. |
| `Vec<T>.remove_at` | `(vector: &mut Vec<T>, index: U64) -> Result<T, U64>` | Take out the element at `index`; those after it move down one. |
| `Vec<T>.insert_at` | `(vector: &mut Vec<T>, allocator: &Allocator, index: U64, item: T) -> Result<(), T>` | Put `item` in at `index` (up to the length); those from there move up one. |
| `Vec<T>.reverse` | `(vector: &mut Vec<T>) -> ()` | The elements in the opposite order. |
| `Vec<T>.extend_copy_mut` | `(vector: &mut Vec<T>, allocator: &Allocator, other: &Vec<T>) -> Result<(), U64>` | Append a copy of each of `other`'s elements, in order. |
| `Vec<T>.push_all_copy_mut` | `(vector: &mut Vec<T>, source: &Vec<T>) -> Result<(), U64>` | Append every element of `source` into room already there, in one copy. |
| `Vec<T>.clear_mut` | `(vector: &mut Vec<T>) -> ()` | Remove every element, keeping the room. |
| `Vec<U8>.filled_ascii` | `(allocator: &Allocator, count: U64, byte: U8) -> Result<Vec<U8>, U64>` | Build a UTF-8 prefix with one range fill, rejecting non-ASCII input. |
| `Vec<U8>.utf8_boundary` | `(value: &Vec<U8>, index: U64) -> Bool` | Check whether an initialized byte can begin a UTF-8 scalar. |
| `Vec<U8>.truncate_utf8_mut` | `(vector: &mut Vec<U8>, length: U64) -> ()` | Keep a shorter prefix in place, provided it ends at a scalar boundary. |
| `Vec<U8>.append_ascii_spare` | `(vector: &mut Vec<U8>, byte: U8) -> Result<(), U64>` | Write one ASCII byte into already reserved space. |
| `Vec<U8>.append_spare_utf8` | `(vector: &mut Vec<U8>, source: &str) -> Result<(), U64>` | Append text into reserved space while preserving the UTF-8 prefix. |
| `Vec<U8>.extend_from_slice_mut` | `(vector: &mut Vec<U8>, allocator: &Allocator, source: &[U8]) -> Result<(), U64>` | Append the bytes of source in order. |

## `import hashmap` (library/wasm-hashmap.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `HashMap<K, V>.new` | `(allocator: &Allocator, capacity: U64) -> Result<HashMap<K, V>, U64>` | An empty map of `capacity` cells. |
| `HashMap<K, V>.for_entries` | `(allocator: &Allocator, entries: U64) -> Result<HashMap<K, V>, U64>` | A map that holds `entries` entries without growing: the cells `put`'s three-quarters rule asks for, `ceil(4 * entries / 3)`, rounded up to odd (`new` takes cells, not entries). |
| `HashMap<K, V>.len` | `(map: &HashMap<K, V>) -> U64` | How many entries it holds. |
| `HashMap<K, V>.capacity` | `(map: &HashMap<K, V>) -> U64` | How many cells it has. |
| `HashMap<K, V>.find` | `(map: &HashMap<K, V>, key: &K) -> Result<U64, U64>` | Ok: the cell that holds this key. |
| `HashMap<K, V>.find_by` | `<Q: Any + Hash + Equivalent<K>>(map: &HashMap<K, V>, key: &Q) -> Result<U64, U64>` | `find` for a key of any type that compares with keys and hashes as they do (`Equivalent<K>`): a Text literal finds a String key without making a String. |
| `HashMap<K, V>.contains` | `<Q: Any + Hash + Equivalent<K>>(map: &HashMap<K, V>, key: &Q) -> Bool` | Whether a value is stored under this key. |
| `HashMap<K, V>.get` | `<Q: Any + Hash + Equivalent<K>>(map: &HashMap<K, V>, key: &Q) -> Result<&V, U64>` | A reference to the value stored under this key. |
| `HashMap<K, V>.get_mut` | `<Q: Any + Hash + Equivalent<K>>(map: &mut HashMap<K, V>, key: &Q) -> Result<&mut V, U64>` | A reference to the value stored under this key, for changing it where it is. |
| `HashMap<K, V>.entry` | `(map: &HashMap<K, V>, slot: U64) -> Result<&Entry<K, V>, U64>` | The entry in one cell, for walking the map in time proportional to its capacity: `for slot in 0..map.capacity():` and skip the cells that answer Err (empty). |
| `HashMap<K, V>.next` | `(map: &HashMap<K, V>, cursor: &mut U64) -> Result<&Entry<K, V>, U64>` | For `for entry in &map:`. |
| `HashMap<K, V>.insert` | `(map: &mut HashMap<K, V>, key: K, value: V) -> Result<(), Entry<K, V>>` | Store a new key. |
| `HashMap<K, V>.place` | `(map: &mut HashMap<K, V>, entry: Entry<K, V>) -> Result<(), Entry<K, V>>` | Store an entry whose key is not present. |
| `HashMap<K, V>.remove` | `<Q: Any + Hash + Equivalent<K>>(map: &mut HashMap<K, V>, key: &Q) -> Result<Entry<K, V>, U64>` | Take a key and its value out. |
| `HashMap<K, V>.take_any` | `(map: &mut HashMap<K, V>) -> Result<Entry<K, V>, U64>` | Take any one entry out, for emptying the map before it is released. |
| `HashMap<K, V>.take_next` | `(map: &mut HashMap<K, V>, cursor: &mut U64) -> Result<Entry<K, V>, U64>` | Take entries out one after another: `while map.take_next(&mut cursor)` empties the map in one pass over its cells (calling `take_any` until Err scanned from cell 0 each time). |
| `HashMap<K, V>.resize` | `(input: HashMap<K, V>, allocator: &Allocator, capacity: U64) -> Result<HashMap<K, V>, HashMap<K, V>>` | Move every entry into a table of the requested capacity. |
| `HashMap<K, V>.put` | `(map: &mut HashMap<K, V>, allocator: &Allocator, key: K, value: V) -> Result<(), Entry<K, V>>` | Store a new key, moving to a table twice the size when this one is three quarters full. |
| `HashMap<K, V>.try_release` | `(input: HashMap<K, V>) -> Result<(), HashMap<K, V>>` | Release a map that holds no entries. |
| `HashMap<K, V>.release` | `(input: HashMap<K, V>) -> ()` | Release a map that holds no entries; one that still holds some stops the run (a trap). |
| `HashMap<K, V>.get_or_insert` | `<Q: Any + Hash + Equivalent<K> + KeyFor<K>>(map: &mut HashMap<K, V>, allocator: &Allocator, query: &Q, value: V) -> Result<&mut V, Missed<K, V>>` | The value stored under `query`, to change in place, or `value` stored under the key `query` makes, after one search either way (`counts.get_or_insert(&allocator, &word, 0)` then `*found += 1`; a get_mut then a put searched twice for each new key). |

## `import keymap` (library/wasm-keymap.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `KeyMap<K, V>.new` | `(allocator: &Allocator, capacity: U64, policy: Policy) -> Result<KeyMap<K, V>, U64>` | An empty map of `capacity` cells. |
| `KeyMap<K, V>.for_entries` | `(allocator: &Allocator, entries: U64, policy: Policy) -> Result<KeyMap<K, V>, U64>` | A map that holds `entries` entries without growing: the cells `put`'s three-quarters rule asks for, `ceil(4 * entries / 3)` (`new` takes cells, not entries). |
| `KeyMap<K, V>.len` | `(map: &KeyMap<K, V>) -> U64` | How many entries it holds. |
| `KeyMap<K, V>.capacity` | `(map: &KeyMap<K, V>) -> U64` | How many cells it has. |
| `KeyMap<K, V>.find` | `(map: &KeyMap<K, V>, key: &K) -> Result<U64, U64>` | Ok: the cell that holds this key. |
| `KeyMap<K, V>.find_by` | `<Q: Any + HashKey + Equivalent<K>>(map: &KeyMap<K, V>, key: &Q) -> Result<U64, U64>` | `find` for a key of any type that compares with keys and hashes as they do (`Equivalent<K>`): a Text literal finds a String key without making a String. |
| `KeyMap<K, V>.contains` | `<Q: Any + HashKey + Equivalent<K>>(map: &KeyMap<K, V>, key: &Q) -> Bool` | Whether a value is stored under this key. |
| `KeyMap<K, V>.get` | `<Q: Any + HashKey + Equivalent<K>>(map: &KeyMap<K, V>, key: &Q) -> Result<&V, U64>` | A reference to the value stored under this key. |
| `KeyMap<K, V>.get_mut` | `<Q: Any + HashKey + Equivalent<K>>(map: &mut KeyMap<K, V>, key: &Q) -> Result<&mut V, U64>` | A reference to the value stored under this key, for changing it where it is. |
| `KeyMap<K, V>.entry` | `(map: &KeyMap<K, V>, slot: U64) -> Result<&Entry<K, V>, U64>` | The entry in one cell, for walking the map in time proportional to its capacity: `for slot in 0..map.capacity():` and skip the cells that answer Err (empty). |
| `KeyMap<K, V>.next` | `(map: &KeyMap<K, V>, cursor: &mut U64) -> Result<&Entry<K, V>, U64>` | For `for entry in &map:`. |
| `KeyMap<K, V>.insert` | `(map: &mut KeyMap<K, V>, key: K, value: V) -> Result<(), Entry<K, V>>` | Store a new key. |
| `KeyMap<K, V>.place` | `(map: &mut KeyMap<K, V>, entry: Entry<K, V>) -> Result<(), Entry<K, V>>` | Store an entry whose key is not present. |
| `KeyMap<K, V>.remove` | `<Q: Any + HashKey + Equivalent<K>>(map: &mut KeyMap<K, V>, key: &Q) -> Result<Entry<K, V>, U64>` | Take a key and its value out. |
| `KeyMap<K, V>.take_any` | `(map: &mut KeyMap<K, V>) -> Result<Entry<K, V>, U64>` | Take any one entry out, for emptying the map before it is released. |
| `KeyMap<K, V>.take_next` | `(map: &mut KeyMap<K, V>, cursor: &mut U64) -> Result<Entry<K, V>, U64>` | Take entries out one after another: `while map.take_next(&mut cursor)` empties the map in one pass over its cells (calling `take_any` until Err scanned from cell 0 each time). |
| `KeyMap<K, V>.resize` | `(input: KeyMap<K, V>, allocator: &Allocator, capacity: U64) -> Result<KeyMap<K, V>, KeyMap<K, V>>` | Move every entry into a table of the requested capacity. |
| `KeyMap<K, V>.put` | `(map: &mut KeyMap<K, V>, allocator: &Allocator, key: K, value: V) -> Result<(), Entry<K, V>>` | Store a new key, moving to a table twice the size when this one is three quarters full. |
| `KeyMap<K, V>.try_release` | `(input: KeyMap<K, V>) -> Result<(), KeyMap<K, V>>` | Release a map that holds no entries. |
| `KeyMap<K, V>.release` | `(input: KeyMap<K, V>) -> ()` | Release a map that holds no entries; one that still holds some stops the run (a trap). |

## `import sort` (library/wasm-sort.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `sort_after` | `<T: Any + Before>(a: &T, b: &T) -> Bool` | `a > b` and `a <= b` on a program's type: whether `b` is before `a`, with `a` evaluated first, as written (red-team review: `b.before(&a)` evaluated `b` first). |
| `total_key` | `(value: F64) -> U64` | A U64 whose order is the F64's totalOrder: a negative's bits reversed, a positive's above every negative's. |
| `precedes` | `<T: Any + Before>(items: &Vec<T>, first: U64, second: U64) -> Bool` | Whether the element at `first` comes before the one at `second`. |
| `sort` | `<T: Any + Before>(items: &mut Vec<T>) -> ()` | Sort in place, least first. |
| `is_sorted` | `<T: Any + Before>(items: &Vec<T>) -> Bool` | Whether each element is not before the one ahead of it. |
| `search` | `<T: Any + Before>(items: &Vec<T>, key: &T) -> Result<U64, U64>` | Where `key` is in `items`, which are sorted least first: Ok(an index holding an element neither before nor after it), or Err(where it would go to keep them sorted). |
| `index_of` | `<T: Any + Eq>(items: &Vec<T>, key: &T) -> Result<U64, U64>` | The first index whose element equals `key`. |

## `import deque` (library/wasm-deque.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `Deque<T>.len` | `(deque: &Deque<T>) -> U64` | How many elements it holds. |
| `Deque<T>.capacity` | `(deque: &Deque<T>) -> U64` | How many it has room for. |
| `Deque<T>.push_back_mut` | `(deque: &mut Deque<T>, item: T) -> Result<(), T>` | Append at the back. |
| `Deque<T>.push_front_mut` | `(deque: &mut Deque<T>, item: T) -> Result<(), T>` | Put in at the front. |
| `Deque<T>.pop_front_mut` | `(deque: &mut Deque<T>) -> Result<T, U64>` | Take the front element out. |
| `Deque<T>.pop_back_mut` | `(deque: &mut Deque<T>) -> Result<T, U64>` | Take the back element out. |
| `Deque<T>.get` | `(deque: &Deque<T>, index: U64) -> Result<T, U64>` | A copy of the element `index` places from the front. |
| `Deque<T>.at` | `(deque: &Deque<T>, index: U64) -> Result<&T, U64>` | A reference to the value `index` places from the front, for `for item in &deque:`. |
| `Deque<T>.new` | `(allocator: &Allocator, requested: U64) -> Result<Deque<T>, U64>` | An empty deque with room for `requested` elements. |
| `Deque<T>.try_release` | `(input: Deque<T>) -> Result<(), Deque<T>>` | Release a deque that holds nothing. |
| `Deque<T>.reserve` | `(input: Deque<T>, allocator: &Allocator, requested: U64) -> Result<Deque<T>, Deque<T>>` | Room for at least `requested` values, in a new allocation if this one is smaller. |
| `Deque<T>.refill` | `(from: Deque<T>, into: Deque<T>) -> Deque<T>` | Move every value of `from` to the back of `into`, which has room for them, and release `from`. |
| `Deque<T>.release_empty` | `(input: Deque<T>) -> ()` | Release a deque that holds nothing. |
| `Deque<T>.reserve_mut` | `(deque: &mut Deque<T>, allocator: &Allocator, requested: U64) -> Result<(), U64>` | Make room for at least `requested` values. |
| `Deque<T>.room_mut` | `(deque: &mut Deque<T>, allocator: &Allocator) -> Result<(), U64>` | Room for one more, growing to twice the capacity and four more when full. |
| `Deque<T>.push_back` | `(deque: &mut Deque<T>, allocator: &Allocator, item: T) -> Result<(), T>` | Add `item` at the back, growing if full. |
| `Deque<T>.push_front` | `(deque: &mut Deque<T>, allocator: &Allocator, item: T) -> Result<(), T>` | Add `item` at the front, growing if full. |
| `Deque<T>.close` | `(input: Deque<T>) -> ()` | The cells outside the deque are empty already, so only the deque's are emptied. |

## `import heap` (library/wasm-heap.wy)

| Function | Signature | What it does |
| --- | --- | --- |
| `Heap<T>.new` | `(allocator: &Allocator) -> Result<Heap<T>, U64>` | An empty heap. |
| `Heap<T>.len` | `(self: &Heap<T>) -> U64` | How many elements it holds. |
| `Heap<T>.is_empty` | `(self: &Heap<T>) -> Bool` | Whether it holds none. |
| `Heap<T>.first` | `(self: &Heap<T>) -> Result<&T, U64>` | The element that `pop` would take. |
| `Heap<T>.push` | `(self: &mut Heap<T>, allocator: &Allocator, item: T) -> Result<(), T>` | Add an item. |
| `Heap<T>.pop` | `(self: &mut Heap<T>) -> Result<T, U64>` | Take the element that comes before all the others. |
| `Heap<T>.into_vec` | `(self: Heap<T>) -> Vec<T>` | The elements as a Vec, in no particular order. |


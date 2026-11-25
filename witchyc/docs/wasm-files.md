# Files through `Dir` and `File`

```
import fs

fn load_config(dir: &Dir<Read>, allocator: &Allocator) -> Result<String, FsError>:
    let file: File<Read> = dir.open("config.txt")?
    return file.read_text(allocator)
```

A `Dir<Read>` or `Dir<Write>` is authority over one directory subtree, with one
right (book: capabilities-authority.md, cookbook-files.md). `main` is the only
place a program gets one; the host decides what it grants:

```
witchy-wasmtime-host program.wasm --output --allocator --dir=/abs/path --dir-rights=read,write
```

A program that needs one file takes it directly, `main(config: File<Read>, out:
File<Write>)`, and the run grants exactly those with `--file=PATH` and
`--write-file=PATH`: no directory at all (`fs/file-in-main.wy`). A program asking
for a file or right the run does not grant does not start.

`--dir-rights` is `read` (the default), `write`, or `read,write`. `--write-dir=PATH`
roots `Dir<Write>` at another directory, so a program can read one tree and write
another (`programs/wordcount.wy`). A program whose
`main` asks for a right the run does not grant stops before it does anything.
`--fs-bytes=N` bounds the bytes a run reads and writes there (1 MiB by default).

For a single run, `--read-dir-alias=NAME:fd:N` lets the initial read-only
`--dir` open an explicitly inherited directory via `dir(NAME)`. Aliases
are exact child names and require `--no-links`. They grant read access
and are unavailable in connection workers, test runs, and runs holding
secrets or `Exec`.

## What the types promise

- **A right is in the type.** `Dir<Read>` opens files for reading and nothing else:
  `create`, `make_dir` and `write_text` are methods of `Dir<Write>` only, and a
  `File<Read>` has no write operation, raw or not (`fs/rights.refused.wy`,
  `fs/raw-right.refused.wy`). A program that reads and writes takes both,
  `main(read: Dir<Read>, write: Dir<Write>)`, and each is visible at every call.
- **More authority stands in for less.** The book's bare `Dir` has both rights
  (`--dir-rights=read,write`). It has every method of both views, and is passed
  where a `Dir<Read>` or `Dir<Write>` is expected, by value or by `&`: the callee
  gets only what its parameter names. Never the reverse: "a Dir<Read> is not a
  Dir<Write>: a capability gives up rights, never gains them" (`fs/full.wy`,
  `fs/widen.refused.wy`). An open tells the host which right it asks for, and the
  host checks the handle has it, so the host enforces what the types say even for
  a full `Dir` passed as a read-only view.
- **Narrowing by reach.** `dir.dir("notes")` is a `Dir` of the same right over
  `notes/` alone. A helper given it cannot name anything outside, `..` included
  (`fs/narrow.wy`).
- **No forging.** No source makes a `Dir` or `File`. They come from `main`'s
  parameters, as the run grants them, or from a `Dir`, and both are affine
  like the console: moved, never copied. A sealed record may hold one
  (docs/wasm-capabilities.md), and other files cannot reach that field.
- **Writes are whole, and never lost silently.** A `File<Write>` collects its
  bytes in the host; `finish` writes a new sibling and renames it over the path,
  so a reader sees the old contents or the new; `abandon` writes nothing
  (`fs/write-read.wy`). A `File<Write>` is linear: one that is neither finished
  nor abandoned is refused, naming the binding (`fs/unfinished.refused.wy`). A
  `File<Read>` or `Dir` may be dropped.

## What the host enforces

The host checks every path again, whatever the program's types say: a path is
plain names separated by `/`. Empty components, `.`, `..`, absolute paths,
backslashes, NUL bytes and paths over 1024 bytes are refused as written, even where they
would stay inside; no symbolic link is followed at any component, even one that stays
inside the grant (each name is opened by cap-std from the directory before it without
following a link) (`fs/escape.wy`, which includes a link pointing out of the granted
directory; `fs/escape-inner-link.wy`, links that stay inside; `fs/swap-during-run.wy`, a
directory on a used path swapped for a link while the program runs).
Each handle is checked for its kind and right on every operation, so a module
built by something other than this compiler gets no more than the grant.

Each operation is one import of type `(i64, i64) -> (i64)`. A path is sent a byte at a
time (`raw_path_byte`); contents move in bulk: `file.read_some(&mut buffer)` and
`file.write_bytes(&bytes)` hand the host a checked byte slice, which it fills or reads in
one call, within the program's memory (memory 0, exported for this; the allocator's never
is). `read_some` says how many bytes it filled, 0 at the end (`fs/bulk.wy`); a read needs a
`&mut [U8]` (`bulk-read-shared.refused.wy`), and the right decides which exist
(`bulk-write-on-read.refused.wy`). `library/wasm-fs.wy` builds the rest from those in
ordinary checked Witchy. Wasmtime's read handle holds the byte snapshot admitted at open
under the file byte budget. `snapshot_remaining()` gives the count of unread bytes
in that handle without advancing it or opening a path. Both byte and bulk reads
advance the same cursor. Use the count to frame a response while copying the
body through a bounded buffer. This operation requires a read handle and a
Wasmtime host that supports the file snapshot import. Native file readers
currently provide streaming reads.

Use `file.read_to_end_into(output)` when the whole input must fit in a caller
buffer. It borrows the file. Use `file.read_text_bounded(allocator, limit)`
for owned UTF-8 text with a caller-specified byte limit. It consumes and closes
the file on success and on returned errors. Both accept input at exactly the
limit and return `TooLarge` if a one-byte probe finds more. The probe consumes
that byte and leaves a caller buffer holding the prefix. The byte limit is
independent of the host's filesystem grant and, for bounded text, the heap
needed for the text and temporary storage.

On native targets, a broker or operating-system read failure stops the run
with exit status 2. It cannot be returned to a file reader as EOF.

| `Dir<Read>` | `Dir<Write>` | `File<Read>` | `File<Write>` |
|---|---|---|---|
| `open`, `dir`, `read_text`, `exists`, `is_dir`, `list` | `create`, `append`, `create_new`, `dir`, `make_dir`, `remove`, `rename`, `write_text` | `read_some`, `next_byte`, `snapshot_remaining`, `read_to_end_into`, `read_text`, `read_text_bounded`, `close` | `write_bytes`, `write_byte`, `write`, `write_string`, `finish`, `abandon` |

`create_new` finishes by hard-linking the written sibling into place, which fails
if anything is at the name by then, so of two writers racing for one name exactly
one wins (`fs/more.wy`). `append` is described below. `remove` removes files, not
directories. `rename(from, to)` moves a file or directory to another path in the
same `Dir`, replacing a file at `to`; both paths are resolved below the `Dir` a
component at a time, so neither can leave it or pass through a link, and a link
itself is not moved (`fs/rename.wy`; the host keeps the first path, sent before
`raw_rename_from`, while the second is built). The `raw_*` operations are available to any program and are as safe as
the library's: the host refuses a failed or closed handle.

## Error codes, and a budget that is used up

What a failed operation gives the program, as `fs_status` and the packed results carry it and
`FsError` names it (`library/wasm-fs.wy`):

| Code | `FsError` | When |
| --- | --- | --- |
| 1 | `NotFound` | nothing at that path |
| 2 | `InvalidPath` | not a plain relative path below the directory, over 1024 bytes, or a name the grant keeps back |
| 3 | `TooLarge` | **a limit of the run**: the byte budget (`--fs-bytes`) cannot cover the read, write, name made or moved; or the run has made its `--fs-entries` names |
| 4 | `WrongKind` | a directory where a file was wanted, or the reverse |
| 5 | `Io` | the operating system refused |
| 6 | `AlreadyExists` | `make_dir` or a new file where a name exists |

`TooLarge` is the only code a limit gives, and a limit gives no other: it is never `Io`.
It does not go away: the budgets count what crossed in the whole run and nothing is given
back, so every later operation that needs bytes gets it too. The engine says which limit
once (`witchy: limit reached: file bytes: ...`; every line of the engine's begins `witchy: `). What the program does is its own: a tool returns;
a server that would only answer errors from then on should stop accepting (the run ends,
and whatever supervises it starts it again with its budgets whole) rather than answer 500
for ever. The network's codes are the same idea (`NetError`, `library/wasm-net.wy`): 1
`NotGranted`, 2 `InvalidAddress`, 3 `TooLarge` (the `--net-bytes` budget is used up, from a
send, a receive, and from `accept`, which then takes no connection from the queue and
answers after its whole two-second wait), 4 `Refused`, 5 `Io`, 6 `TimedOut`, 7 `WouldBlock`.
The byte-at-a-time calls (`next_byte`) have no codes and only fail.

`--usage` (Grimoire always passes it) makes the engine's last line `limit: used: ...`, each
count beside its limit: see docs/decisions.md, 2026-11-22, "What a run used".

## Data names (`--data-only`)

A launcher that grants a directory as data (Grimoire does, for every `[dirs]` grant) passes
`--data-only`, and the engine then says which names a path may have, not which it may not.
This is the one statement of the rule; Grimoire's scan of the directory before a launch
applies the same one.

- **A data name** is 1 to 255 bytes, each an ASCII letter, a digit, `-`, `_` or `.`, and the
  first is not `.`, nor is the last (some filesystems open `a.wy.` as `a.wy`; review of
  2026-11-22). Every component of every path a program gives must be one, for every
  operation: open, create, replace, append, `create_new`, `make_dir`, `remove`, `rename` (both
  paths), `exists`, `is_dir`, and each directory on the way. Anything else is `InvalidPath`.
- **A protected name** is `witchy.toml`, `witchy.lock`, or a name ending `.wy`, `.policy`,
  `.packages` or `.grants.toml`, compared without ASCII case (`Main.WY` is one). Where the run
  may write the directory, a protected name is not a data name: it is not made, replaced,
  moved to, moved from, removed, read or entered. A directory the run only reads may hold and
  read them (nothing there is planted by this run).
- **`list`** shows the entries the program could name and no others: no hidden entry, no name
  outside the set, no protected name where the run may write.
- **Why a positive set.** The first rule listed what to refuse (a leading `.`, the protected
  names lowercased in ASCII), and APFS resolved names it did not list onto the ones it did:
  `witchy.loc` + U+212A KELVIN SIGN opens `witchy.lock`, `main.grant` + U+017F LONG S + `.toml`
  opens `main.grants.toml`. A list of code points to refuse cannot be finished (a file system
  chooses its own folding and normalization), so no byte outside ASCII is in a name at all,
  and within ASCII the only folding is of case, which the comparison ignores.
- **What it costs.** A program granted a directory cannot name a file with a space or a
  letter outside ASCII in it; such files in a directory only read are not seen. Without
  `--data-only` (the engine run by hand) names are as before: any UTF-8 without `/`, `\`,
  NUL, `.` or `..`.

## Unfinished writes (`.witchy-staging`)

`create`, `create_new` and `write_text` write a file whole under another name, sync it, and
rename (or, for `create_new`, hard-link) it into place. Without `--data-only` that name is a
hidden sibling, `.NAME.witchy-PID-N`. Under `--data-only`, in a directory the run may write,
it is `PID-N` in the engine's own directory `.witchy-staging` at the top of the granted
directory, made with mode 0700 at the first write (one that exists must be this user's with exactly no group or other bits, or nothing runs). The granted directory is one device (the
engine enters no other), so the rename out of it is still atomic.

A run killed between the write and the rename leaves the temporary. The next engine to open
the directory removes it before `main` runs. It first checks the staging directory is what an
engine made: opened without following a link, a directory, on the granted directory's
device, this user's, writable by no one else, holding only regular files with one name each
(`nlink == 1`) named `PID-N`. Anything else is refused and nothing runs; nothing is removed
that the pattern does not describe. Engines sharing a directory each hold a shared `flock` on
the staging directory while they run; only an engine that gets the exclusive lock, so the
only one there, clears it. No program can name `.witchy-staging` (a data name does not begin
with `.`) or see it in a listing.

## Append

`dir.append(path)` gives a `File<Write>` whose bytes are added at the end of the file at
`finish`; `abandon` adds nothing. The signature is what it was; what changed (2026-11-22) is
that the file is no longer read and rewritten.

- **A file that is there** is opened at `finish`, from its directory, without following a
  link, with `O_APPEND`; everything written to the handle goes in one `write`, then the file
  is synced (`fsync`, as a replaced file is before its rename). `finish` returns after that.
  The bytes land after whatever the file holds at that moment, so two appenders never
  overwrite each other.
- **Charged:** the bytes added, as they are written to the handle. Nothing for the bytes
  already in the file, and no name is made or moved, so no entry cost. (Before: the whole
  file was read and charged on every append; 199 appends of 50 bytes used a 1 MiB budget.)
- **A file that is not there** (when the handle was opened, or by `finish`) is made whole at
  `finish` by a staged write moved to the name *only if nothing is there*: it appears with its
  first contents or not at all, and costs one entry. If another writer made the file
  meanwhile, the bytes are added to theirs with `O_APPEND` like any other append. An append
  never replaces a file, whenever its handle was opened: three handles opened while `log` was
  absent each add their record (review of 2026-11-22: they each replaced it; test
  `appenders_opened_while_the_file_was_absent_never_overwrite_each_other`).
- **Made only where nothing is** (`create_new`, and an append's first file) is one exclusive
  rename (`renamex_np(RENAME_EXCL)`, `renameat2(RENAME_NOREPLACE)`): the file never has a
  second name. Only on a filesystem without that rename does the engine fall back to a hard
  link and the removal of the temporary.
- **Refused:** a link (`InvalidPath`); a file with a second name, since the engine never
  writes through a hard link (`InvalidPath`); a directory or device (`WrongKind`); a
  protected or non-data name under `--data-only`. A secret's source stops the run, as for a
  replacement.
- **Torn writes.** A replacement is atomic; an append is not. A run killed, a machine that
  loses power, or a disk that fills during the `write` can leave a *prefix* of the appended
  bytes at the end of the file, and bytes not yet synced can be lost with the power. What is
  promised: bytes from a `finish` that returned `Ok` are in the file and were synced; bytes
  from a `finish` that did not return may be there whole, in part, or not at all; nothing
  before them is changed. So a log's reader takes a torn last record as the end of the log
  (a length or checksum per record), and a writer that must not leave one replaces the file
  instead.

## The compiler side

One general mechanism, not one case per operation: `Ty::Host` (a directory or file
with its right) and `Value::Host(op, receiver, operands)` in both IRs, with the
table of operations and which right each needs in `src/compiler/host.rs`. Every pass
treats a host operation like the console's: a read of its receiver through a
checked loan, and an effect. `impl Dir<Read>` and `impl Dir<Write>` are separate
namespaces of methods, as the two consoles are.

## Still trusted, and not yet

- A `Dir` is a cap-std `Dir` in the host, opened once when the run starts, and each
  path is opened from it a name at a time without following a link (src: `Files` in
  tools/wasmtime-host). Another process that renames or replaces directories
  while the run works cannot redirect an open outside the grant; a `Dir` keeps
  meaning the directory it was opened as. cap-std keeps every step below the `Dir`
  as well, so a name check the host got wrong would still not leave the grant.
- `read_text` needs an allocator for the text; the book's version reads into a
  string without one in view. Contents cross 256 bytes per call (`read_some` into a
  buffer of the caller's size is the call under it). The text is built in a `Vec<U8>`
  that doubles as it fills, so while it grows the old and the new buffer are both
  alive: a file needs up to about three times its size of heap for a moment, and
  `read_text` answers `OutOfMemory` (an `FsError`, not "no such file") when the heap
  does not have it. With the default heap that is somewhere between 33 KiB (read) and
  48 KiB (refused), measured by the second dogfooding report. A larger file is read a
  window at a time with `open` and `read_some`, or the run is given a larger heap. (`[run] heap-pages` under Grimoire: docs/wasm-memory-sizes.md).
- `list` gives the names as one `String`, a line each.
- Paths are `Text` literals in `open`, `create`, `dir` and the rest; a path held in
  a `String` (a name from `list`) goes to `open_path`, `read_text_path`,
  `dir_path` and `create_path`.

## A whole program

`tests/fixtures/programs/wordcount.wy` counts the words of each `.txt` file in one
directory and writes a report to another. `wasm-caps` on it:

```
main: Allocator, Console<Write>, Dir<Read>, Dir<Write>
report: Allocator, Dir<Read>
36 reachable functions hold no capability
```

(The library's own functions, such as `Dir<Read>.list` and the private
`opened: File<Write>`, are listed too; they are left out here.)

`count_words` and `is_text_file` are `pure fn`, so the compiler checked that they
can read no file and write none; `report` can read the input tree and nothing
else.

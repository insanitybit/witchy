# Multi-file packages

Status: shipped 2026-11-22 in Coven and Grimoire (branches `work/multi-file`). A library whose
`source` imports files of its own directory is published, fetched, locked, reviewed and built
as one release that covers every one of those files. The compiler needed no change. This page
is the specification, the decisions and their reasons. The code is `witchy_release.py`, one
file shipped byte for byte with both tools, each of which pins its SHA-384.

## The release

A release is a record (ASCII lines, then an empty line) followed by a body. Its digest is the
SHA-384 of the record followed by the body: what the owner signs through the index entry, what
the operator's snapshot fixes, what the lock pins and what every fetch verifies. There are two
formats, told apart by the first line.

```
witchy-package 1                 witchy-package 2
name NAME                        name NAME
version X.Y.Z                    version X.Y.Z
requires OTHER X.Y.Z   (0+)      requires OTHER X.Y.Z          (zero or more, sorted by name)
<empty line>                     entry PATH
<the one source file>            file PATH LENGTH sha384:HEX   (two or more, sorted by PATH's bytes)
                                 <empty line>
                                 <the files' bytes in the listed order, nothing between or after>
```

- **One file is format 1, two or more are format 2, always.** A package of one file packs to
  exactly the bytes it always did, so every existing digest, lock and signature stands. A
  format 2 record with fewer than two files is refused. So a set of files has one format.
- **One byte string per set of files.** Fixed field order, single spaces, one `\n` per line,
  lengths in decimal without leading zeros (at most 7 digits), lower-case hex, requirements
  sorted by name, files sorted by the bytes of their paths and listed once, a body that is
  exactly the listed lengths. Every reader parses the fields, writes the record again from
  them and refuses bytes that differ, so nothing is canonical by convention only.
- **The digest covers every file twice.** The body is hashed as part of the release, and each
  `file` line carries that file's length and SHA-384, which every reader checks against the
  body. The per-file digests are what a review and a policy line name; the whole digest is
  what is signed. A registry that changes one byte of one file fails the whole digest; one
  that also recomputes the registry's `digest` file fails the owner's signature (or, for an
  unsigned package, the lock); one that leaves the record's `file` line stale fails that line.
- **Never read as each other.** A format 1 body is one file whatever it contains (a body that
  looks like a listing is still one source). A format 2 record is refused by every reader
  written before it, by their own strict checks: Coven's upload and `inspect`
  (`invalid package requirement`) and Grimoire's record check
  (`not the record of NAME VERSION`), so nothing built with an old tool sees a new release.
  An old `coven fetch` run by hand copies the verified bytes (`package`, `source.wy`) and
  interprets nothing; old Grimoire then refuses them. An unknown `witchy-package N` is refused
  with "use a newer tool".
- **SHA-384 for the per-file digests** (the `witchy-2` suite's digest; recorded here as
  Coven's hash-choice note asks): the listing is inside the bytes the release digest covers,
  so its strength is bounded by that digest, and a second algorithm would add a second thing
  to check and no strength. The policy's pins are SHA-384 of the same file bytes, so a `file`
  line, a policy line and a review name one value.

## Paths

Every reader applies all of these, to every path, before it opens or creates anything:

- relative, `/`-separated, at most 8 components and 255 bytes, each component at most 64 bytes;
- a component is runs of `[a-z0-9_]` joined by single `.` or `-`: so no empty component, no
  `.` or `..`, no leading dot, no leading or trailing separator, no `\`, space, NUL, control
  or bidirectional character;
- **lower-case ASCII only.** Chosen over "NFC only": with ASCII there is no normalization
  form for a publisher's and a consumer's file system to disagree about, and with lower case
  no two paths collide on a case-insensitive volume. The cost is that a package of several
  files names them in lower case. Collision under case folding and NFC is also checked on its
  own, so the rule survives a later change of grammar;
- the last component ends in `.wy`. **The allowlist of other kinds is empty**: every file of
  a release must be reached by the entry's imports (below), and only a source can be;
- no path twice, none out of order, and no file that is a directory of another (`a.wy` and
  `a.wy/b.wy`);
- 2 to 64 files (the compiler bundles at most 64 files into a program), each at most 1 MiB,
  and record plus body at most 1 MiB (the existing bound of a version).

## What is in a release

- **The entry and exactly what it reaches.** The entry is `[package].source`; its directory is
  the package's directory, so the entry is always a top-level path of the release. The other
  files are those reached through `import "relative.wy"` lines, read as the compiler reads a
  source's header, resolved by text inside the package's directory.
- **Every reader checks the same closure.** An import that leaves the directory (`..` above it,
  an absolute path), an import of a file the release does not list, a listed file nothing
  imports, and an `import "@other"` of a package the record does not require are each refused,
  at pack time, on upload, on fetch and before staging. A release therefore builds from its
  own files only and holds nothing that is not compiled: no file that a review of the
  program's files would not show.
- **Never a test member.** `test.wy`, `tests/` and anything only they import are not in a
  release; an import of one from the entry's closure is refused.
- **No links at publish time.** `coven pack` and `publish` open the package's directory once
  and every file below it through its parent's handle, one component at a time, without
  following a symbolic link (`O_NOFOLLOW`); each must be a regular file with one name
  (`nlink == 1`) on the directory's device. A file is read once and the bytes read are the
  bytes hashed and packed. This now also holds for a package of one file.

## Who verifies what

| Reader | When | What |
|---|---|---|
| `coven pack`, `publish` | publishing | reads by handle, packs, then parses its own output |
| Coven server (`POST /v1/packages/...`) | upload | parses record and body (everything above) before the owner, index and signature checks; then `publish.wy` hashes and stores |
| `coven fetch` | fetch | `fetch.wy` verifies the signature batch, then checks the coordinate, digest, record boundary and file contents before staging. The launcher checks the cached pair's digest and record boundary. |
| `coven inspect` | by hand | the guest's digest and record, then the whole file parsed |
| `grimoire lock`, `run`, `build`, `review` | every resolve | parses the fetched release and checks its digest against Coven's verified receipt and any applicable lock pin before unpacking it. TUF fetches deliver one payload. Legacy fetches deliver the record and body separately. |
| `grimoire build` of a library | before publishing | unpacks the packed release and compiles an import of it, so a release that would not build is not published by `grimoire` |
| compiler | build | a package's file imports nothing outside its entry's directory; each file is held to the policy by name and digest |

The registry's slot is unchanged (`package`, `source.wy`, `digest`): for format 2, `package` is
the record and `source.wy` the body. `publish.wy` and `fetch.wy` hash the two as one, as they
always have, so the engine's SHA-384 covers every file with no change to either program, and
the HTTP messages and the lock format are unchanged.

## Unpacking

Grimoire unpacks both release formats into a new `files/` directory in its private temporary
tree. It creates each directory and file exclusively through its parent's descriptor and
writes the verified bytes returned by the release parser. Source files have mode 0400. The
generated package list points the compiler at this new tree. For format 1 the entry is
`source.wy`. The compiler uses the entry's directory as the package root.

## Review and policy

- The compiler names a package's entry `@NAME` and its other files `@NAME/PATH`, in a registry
  build and a checkout alike. `grimoire review` prints one capability line per file with a
  function `main` reaches and, for each package of several files, every file of its release
  with its size and digest, so a file with no reachable function is still shown.
- A policy holds the package file by file (what `--write-policy` drafts), or to one ceiling,
  `@NAME/: Allocator ; no unchecked ; digest sha384:D`. A line `@NAME:` covers the entry only:
  a package that grows from one file to several is refused (`@NAME/x.wy is in no line of the
  policy`) until its author writes lines for the new files.
- **The diff on update is per file.** For each registry package the policy pins, `review`
  lists every file added, removed or changed since the accepted digests, with line counts, the
  functions touched and the two copies to compare; a new file is named with where to read it
  whole. The accepted state is what the policy's digests stand for: a file's text for a file
  line, the list of covered files for a directory line, both kept on this machine at
  `--accept` and checked against the digest when read. Without a kept copy or list, every file
  that may differ is named. The diff decides nothing: the compiler refuses a build whose files
  do not match the policy's digests.
- **Every file of every locked release is pinned, whether or not `main` reaches it** (review
  of e593082, finding 1). The compiler asks a policy line only of a file that holds a function
  `main` reaches, so a file of constants or types alone (`pub let FACTOR = 6`) had no line in
  a drafted policy, and a version that changed only that file built and ran with no review.
  Before `run`, `build` or `test` compiles anything, Grimoire now asks of each file of each
  verified release (a release of one file too) that the policy's line for it carries a digest
  and that the digest is of the release's bytes: the file's own for a file line; for a
  directory line the compiler's digest over every file the line governs, which does include
  files no function is reached in (checked by running, and by the gate itself: a difference
  refuses). `review` names each unpinned file, exits 1 and does not print "fits the policy";
  `--write-policy` drafts a pinned `nothing` line for each file the compiler drafted none
  for; `--accept` gives a digest to a line that holds a release's file without one and adds
  the `nothing` line for a file with no reached function. A new file that does hold reached
  code still needs a line its reviewer writes. A registry package held by a line with no
  digest, which ran before, is now refused until accepted.
- `review --accept` re-pins digests and widens nothing, as before. A build record
  (`run --record`) lists each file of a multi-file package with its digest.

## Changes from the design draft (2026-11-22, same day)

1. **The compiler is not changed.** The draft added `; root ROOT` to the package list and new
   policy names. The compiler already takes the entry's directory as the package, refuses
   imports that leave it and names files `@NAME/PATH`; the release puts the entry at the top,
   so the existing rule is the containment.
2. **One file stays format 1.** The draft let a format 2 release hold one file, which gives
   one file two byte strings and changes digests on republish.
3. **Every reader checks the import closure**, not only `pack`. The draft trusted the publisher
   to ship "exactly the files reached"; a hand-made release could carry a file that no review
   of the built program shows.
4. **No separate "tree manifest" digest for the policy.** The draft's `@NAME/` digest was the
   SHA-384 of the record's `entry` and `file` lines. The compiler's directory digest already
   covers each covered file's name and pin; a second definition would be a second thing to
   keep equal.
5. **Extraction is Grimoire's, not `coven fetch`'s.** `fetch.wy` keeps writing the verified
   pair, and the tool that builds unpacks from bytes it parsed itself. Coven still parses every
   release it accepts, serves to a fetch or inspects.
6. **Tighter paths and bounds.** No `..` inside a component, no leading or trailing separator;
   64 files, not 256 (the compiler's bundle limit); the entry must be top-level.
7. **`@other` imports are checked against `requires` by every reader** (the draft left it to
   the compiler).
8. **Coven re-checks what `fetch.wy` kept.** The guest validates and stages the same bytes.
   The launcher checks that the two published cache files still match the verified digest
   and record boundary.

## Not done

- **A file on another device** is refused by `read_file` (`st_dev`), but no test mounts a
  second device.
- **A dependency's tests** are still not in a release (`grimoire test --dependency`).
- **Windows-reserved names** (`con.wy`, `nul.wy`) are allowed; the tools run on macOS and Linux.
- **A checkout's package** (a local `.packages` list) is diffed by its entry file only, as
  before; the per-file diff is for registry releases, whose file set is known.
- **Unreached files of a checkout.** The same hole exists for a checkout package and for the
  program's own secondary files (measured: a listed `@dep` of constants alone, in no line of
  the policy, ran, and ran again changed). Grimoire now refuses a listed package whose first
  file is in no line. Its other files and the program's own are not known to Grimoire and
  wait for the compiler. Cause, at `work/vec-string-core` b89d094b: `caps::file_uses`
  (`src/wasm/caps.rs:124-131`) skips a bundle file with no reached function
  (`if here.is_empty() { continue; }`), and `check_policy` (line 215) and `draft_policy`
  (line 156) iterate that list; `check_digests` and `repin` iterate every bundle file
  (`files::digests`), so a pinned line is enforced once it exists. Fix there: emit a
  `FileUse` with `functions: 0` for every bundle file (or give `check_policy`/`draft_policy`
  the `files::names` list), so "in no line of the policy" and the draft cover all files;
  `--by-file` would then print `0 reachable functions`.
- **An old Grimoire meeting a format 2 release** refuses it, but only after Coven's fetch
  verified the snapshot and index, so it has by then written its trust-store entries (the
  operator, the package's owner and their floors) for that registry. Nothing is built, and
  the entries are those any successful resolve writes; a user who did not mean to trust that
  registry removes them with `grimoire trust --forget`.
- **The import reader is a second implementation** of the compiler's header rule (Python
  beside Rust). A disagreement cannot widen a build (the compiler reads only files of the
  unpacked directory, and each is in the release and its digest), but it can refuse a release
  the compiler would accept or ship a file the compiler does not read.

## Tests

`test_release_format.py` (in both repositories, identical): golden vectors computed with
`shasum`, round trips, every path rule alone and inside a release, every non-canonical spelling
of the golden release, both formats against frozen copies of the readers from before format 2,
packing from a directory (symbolic link, hard link, FIFO, linked directory, escape, test
member, bounds) and unpacking. Coven `test_release_registry.py`: pack, publish and fetch of
several files, the server's refusals, a client's refusal of a tampered file in three ways, and
a slot swapped between check and copy. Grimoire `test_release_review.py`: Grimoire's own check
of what Coven fetched, staging, the policy-line choice and the per-file diff.
`tests/http_registry.py` (the HTTP witness) publishes a multi-file update over the socket.

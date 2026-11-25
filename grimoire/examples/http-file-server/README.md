# HTTP file server from a signed package

`app/` is an executable that serves files over HTTP/1.0 with `@httpd`, a package it takes from a local signed Coven registry. The executable's grant document gives the run one loopback listener, one read-only directory and the clock. The package gets only what `main` passes it for each connection: the socket, a buffer, a read-only handle on `site/public`, a clock and memory. It answers only requests whose `Host` header names the granted listener.

- `httpd/` is the package (`httpd.wy`, Witchy 2's `tests/fixtures/packages/httpd/httpd.wy`; `witchy.toml` names it `httpd 1.0.0`).
- `app/main.wy` accepts connections and runs each as a task, cut off after two seconds.
- `app/main.grants.toml` is everything the run may reach.
- `app/main.policy` holds `@httpd` to `Allocator, Clock, Dir<Read>, Socket`, with no unchecked code, at the reviewed digest.
- `app/site/public/hello.txt` is served. `app/site/private.txt` is inside the grant, but not in what the package is given. `app/secret.txt` sits beside the granted directory.

## Serve a file from the signed HTTP package

Follow the [Coven HTTP quickstart](../../docs/coven-http.md) through operator bootstrap. Keep the service running. With the same shell variables and homes, publish the HTTP library first and snapshot it from that operator's current sequence (shown here as 1).

```sh
cp -R "$src/grimoire/examples/http-file-server/httpd" author/httpd
cp -R "$src/grimoire/examples/http-file-server/app" consumer/web
owner=$(role author grimoire keygen publisher | cut -d' ' -f2)
cat >>author/httpd/witchy.toml <<EOF

[registry]
url = "$url"
operator = "$operator"
id = "demo"
EOF
role author grimoire publish httpd --key publisher
role operator coven snapshot --registry "$url" --key operator --id demo --min-base 1 || true  # review only: nothing signed
# After checking the first claim of httpd is by $owner:
role operator coven snapshot --registry "$url" --key operator --id demo --min-base 1 --admit httpd
cat >>consumer/web/witchy.toml <<EOF

[registry]
url = "$url"
operator = "$operator"
id = "demo"
[dependencies]
httpd = "1.0.0"
[owners]
httpd = "$owner"
EOF
role consumer grimoire lock web
role consumer grimoire run web
```

Before it builds anything, `grimoire run` prints what the run is granted, from the flags it will pass the engine (`grimoire review web` prints the same lines without running):

```
grant: allocator: heap memory inside the module
grant: output: the program writes its standard output
grant: listener: accepts connections on 127.0.0.1:8417 (loopback only), and dials nothing
grant: clock: the time and timers
grant: directory: /.../consumer/web/site, read-only (no symbolic link followed, no other device entered)
limit: stops after 300000 ms
limit: instructions are not counted
```

From a second terminal, run `curl -i http://127.0.0.1:8417/hello.txt`. It returns HTTP 200 and `hello from witchy`. Port 8417 is granted by the app's own document. The registry listens separately on 8787. Stop the app with Ctrl-C; its manifest also sets a five-minute deadline.

The server answers under one name, the granted one. `curl -i http://localhost:8417/hello.txt` and `curl -i -H 'Host: other.example' http://127.0.0.1:8417/hello.txt` get `421 Misdirected Request`; a request with no `Host` header, or two, gets `400 Bad Request`. To serve on IPv6 loopback, grant `listener = "[::1]:8417"` and use `curl -g -i 'http://[::1]:8417/hello.txt'`.

## What the package cannot reach

`tests/http_file_server.py` runs the steps above, then checks what the package cannot do:

```sh
mbx exec python3 tests/http_file_server.py -v
```

- **It cannot read a file outside its directory.** `/../secret.txt`, `/../../secret.txt` and `/../private.txt` get 404. The engine refuses any path with `..` or `.` before opening anything. `/%2e%2e/secret.txt` gets 404, because the package does not decode it. The test also adds links in `site/public`: `leak.txt -> ../../secret.txt` and `alias.txt -> hello.txt`. Both get 404, because the engine follows no link anywhere in a granted tree. The granted directory is opened from `/` without following a link (`--no-links`), so a `site` that is a link, or that has a link above it, is refused too.
- **It cannot be reached under another name.** A page on another site can point its own name at `127.0.0.1` (DNS rebinding) and have a browser send its requests here. The browser sends that name in `Host`, and the package answers only for the listener the run was granted: `http.request_on` asks the accepted socket which listener it came from, and the engine answers from the grant (`127.0.0.1:PORT`, or `[::1]:PORT`). The test sends `rebound.example`, `localhost:PORT`, `127.0.0.1` with no port (421 each), no `Host` and two (400 each). The app passes the package nothing for this and cannot pass it wrong.
- **No other process shares its address.** While the server runs, the test binds `127.0.0.1:PORT` again with no option, `SO_REUSEADDR`, `SO_REUSEPORT` and both; each is refused. The engine turns `SO_REUSEPORT` off on its listener and checks that it is off.
- **It cannot dial out.** The test publishes `httpd 1.1.0` with `phone_home(net: &Net)`, which connects to a canary listener the test holds. Its publisher directory also ships a grant document asking for `[net]`. A Coven release holds only its record and its one source file, so that document never reaches a consumer. Grimoire reads grants only from the executable's own document.
  - The package can reach a `Net` only if `main` passes it one. The test's app does pass one, and its policy allows `Net`.
  - `grimoire run` is still refused before anything is built, with `net: main takes a Net, and the document grants none ([net].net)`.
  - Adding `[net]` to the executable's own document is refused too, before anything is built, with `[net]: grimoire grants no outbound network access`. Any `[net]` section is refused this way, even where `main` takes no `Net`.
  - The canary never receives a connection.
- The `--record` of the honest run lists exactly `--allocator --output --listen=127.0.0.1:PORT --clock --dir=.../app/site --no-links --stream-output --plain-output --deadline-ms=300000 --unmetered --net-bytes=1048576 --fs-bytes=1048576 --output-bytes=65536 --memory-pages=4`.

## The launcher interface

Each grant is approved by name in the executable's grant document (`[grants] document` in `witchy.toml`). `witchyc wasm-grants` binds the entry to `main`'s parameter of that name and proposes the engine flag. Grimoire checks each proposed flag and refuses any flag it does not know.

| Grant document | `main` takes | Engine flag | Grimoire refuses |
| --- | --- | --- | --- |
| `[listen] NAME = "127.0.0.1:PORT"` or `"[::1]:PORT"` | `NAME: Listener` | `--listen=127.0.0.1:PORT`, or `--listen=::1:PORT` (the compiler writes the IPv6 literal in its canonical text and without brackets, which a flag may not hold; the port follows the last `:`) | any host but `127.0.0.1` and `::1` (no names, no wildcard, no other spelling of loopback such as `::ffff:127.0.0.1`); a port that is 0, over 65535, or written with leading zeros; a second listener |
| `[dirs] NAME = { root = "path", rights = ["Read"] }` | `NAME: Dir<Read>` | `--dir=/absolute/path`, plus `--no-links`, which Grimoire always adds | a path that is not absolute and normalized (no `.`, `..` or empty component); any component that is missing, not a directory, or a symbolic link (each opened with `O_NOFOLLOW` from `/`); a directory in or holding a descriptor-alias tree (`/dev`, `/dev/fd`, `/proc`, by device and inode); a directory not on a local disk (APFS, HFS, ext, XFS, Btrfs, tmpfs, ZFS, overlay; so no `/net` or NFS), or with any file system mounted below it (by device and inode, so firmlinked spellings count); any directory grant while the program takes a secret; a second directory; `Dir<Write>` or `Dir` (`--dir-rights=`, `--write-dir=`) |
| `[clock] NAME = true` | `NAME: Clock` | `--clock` | |
| `[paths] links = false` | | `--no-links` | |
| `[net] NAME = [...]` | `NAME: Net` | `--net=...` | always, as a section of the document, whatever `main` takes; and `--net=` if ever proposed: no outbound network |

The relative `root` paths start in the document's directory.

Decision (automated review of d5d6344): no directory is granted while the program takes any secret. A tree can hold a secret's source file under any name or hard link, and only walking it would find one. The engine already refuses a read of a `--secret-object` file through the directory, because `read_whole` checks the device and inode of the file it opened. But a secret from `env:` or a store has no file to compare, and a walk before the run races with the run. So the launcher refuses the grant. The Witchy 2 compiler also refuses a `main` that takes two parameters of one capability type, such as two `Dir<Read>` or two `Listener`.

Run limits sit in `witchy.toml`'s `[run]` table. They are not grants, and an unknown key or a bad value is refused:

| `[run]` | Engine flag |
| --- | --- |
| `deadline-ms = N` (1 to 600000; default 1000), or `"none"` | `--deadline-ms=N`, or `--deadline-ms=none` with a lifeline descriptor |
| `fuel = N` (1 to 10^12; default 1000000), or `fuel = "unmetered"` | `--fuel=N`, or `--unmetered` |
| `fuel-per-connection = N` | `--fuel-per-accept=N` |
| `net-bytes`, `file-bytes`, `output-bytes` (0 to 2^40; defaults 1 MiB, 1 MiB, 64 KiB) | `--net-bytes=N`, `--fs-bytes=N`, `--output-bytes=N` |
| `memory-pages` (4 to 32768; default 4), `module-bytes` | `--memory-pages=N`, `--module-bytes=N` |

Each limit that applies to the run's grants is passed as a number and shown as a `limit:` line; the main README's "Output and limits" has the table. Output is passed on a line at a time (`--stream-output`), so the server's `serving` line is seen while it serves.

A directory with `rights = ["Read", "Write"]` (`main` takes a `Dir`) is granted as `--dir=` with `--dir-rights=read,write`. Any granted directory, read or written, is strictly inside the project (here `app/site`) or named by the operator at launch with `--allow-read-root` / `--allow-write-root`: the main README's "A directory the program may write" has the rule.

## Limits

- The engine's byte budgets default to 1 MiB of network traffic and 1 MiB read from files per run. Past them requests fail, and the engine says so once on standard error. Raise them with `[run] net-bytes` and `file-bytes`.
- A numeric deadline is at most ten minutes. `deadline-ms = "none"` runs until the server is stopped or its launcher ends.
- Grimoire's check of the directory runs before the build. The engine opens the directory again, later, without following links, so a link swapped in after the check is refused, not followed. A directory replaced by another real directory in that window is the one served.
- The mount check runs before the run, and the engine keeps to it during the run: each directory and file it opens below the granted one must be on the device of the directory it was opened from, compared on the handle just opened (`fstat`), so a file system mounted inside the tree after the check is refused like a link (404 here), not entered. Checked by hand with a disk image attached inside `site/public` while the server ran: 200 before, 404 for a file on the image after. A nested file system that was meant to be served (a Btrfs subvolume, say) needs its own grant. The comparison is by device number, so on Linux a bind mount of another directory of the same file system, made inside the tree during the run, is not told apart; that needs `openat2` with `RESOLVE_NO_XDEV` or mount ids in the engine, and is open.
- A hard link to `secret.txt` placed inside `site/public` would be served. Making one needs write access to the granted directory, which the package does not have.

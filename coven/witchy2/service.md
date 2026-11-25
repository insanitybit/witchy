# Operating the loopback registry

Install the `coven` executable with its sibling `coven_http.py`, `witchy_compiler.py` and `.wy` files. Put a compatible `witchyc` and `witchy-wasmtime-host` on PATH and select the installed proof toolchain. The matching Grimoire checkout contains a complete installation and signed first-publication walkthrough in `docs/coven-http.md`.

`coven serve --store STORE --port PORT` binds to IPv4 loopback. Port 0 chooses an available port and prints the URL. STORE must be an owner-only directory. The server maintains its own SQLite database inside it. Restart with the same store, URL and port to retain package versions, indexes, operator rotations and snapshots.

Initialize the operator's seed and public key outside the store. Run `coven snapshot --registry STORE --key OPERATOR --first --id ID` as the explicit first operator. Give clients the URL, that verified public key ID and the registry ID out of band. After a signed `grimoire publish`, run `coven snapshot --registry URL --key OPERATOR --id ID --min-base N` with the latest snapshot sequence the operator knows it signed. The operator reviews current signed package heads and signs locally. Before signing, the command lists every change since the operator's last snapshot (`review:` lines): each first claim of a package name with its owner key, recovery key and versions, and for admitted packages the new index, added versions, withdrawals and owner rotations. It signs nothing unless each first claim is named with `--admit NAME` (repeatable), and it refuses an `--admit` that names no first claim. New versions, withdrawals, refreshes and rotations of an admitted package need no admission: the owner's signature chain and the operator's admission rules (`admit`) already decide them. A snapshot taken through the server store (`--registry STORE`) reviews the same way, since its contents also arrived over HTTP. The server checks the active signer, registry, predecessor, increasing sequence, expiry, admitted indexes and handovers before committing. Established withdrawal, recovery and rotation commands can be applied by authorized parties through the private-store administration interface. Access to that interface is local administration. An HTTP client never supplies a filesystem path to the server.

## Version 1 HTTP objects

| Method and path | Result or action |
|---|---|
| GET `/v1/snapshot` | Current signed snapshot and operator rotations |
| GET `/v1/catalog` | A representative published coordinate per package for operator review |
| GET `/v1/packages/NAME/head` | Current signed package index and package rotations |
| GET `/v1/packages/NAME/index/INDEX/CHAIN` | The immutable index and rotations named by the consumer's snapshot |
| GET `/v1/packages/NAME/VERSION/INDEX/CHAIN` | Release record, source, digest, signature and the immutable index and rotations named by the consumer's snapshot |
| POST `/v1/packages/NAME/VERSION` | Add the signed release and its owner-signed index against an expected snapshot and previous index |
| POST `/v1/snapshot` | Install an independently signed successor to the identified current snapshot |

Messages are bounded JSON objects. Binary fields carry Base64 of the existing canonical bytes. An absent optional object is JSON null. The transport never re-encodes a signed proposition or accepts a caller-selected filename. Clients verify the release, source digest, owner history, exact snapshot-bound index, operator chain and remembered floors locally. A valid digest alone cannot authorize a different owner or registry. A release becomes visible for resolution when an operator snapshot admits it.

The Python launcher accepts loopback connections and handles transactional storage. Witchy workers read requests, write replies, and execute publication, signing, verification and extraction in the separate Wasmtime runtime with explicitly granted files and directories. The author transmits signatures and public objects. The operator's seed stays in the operator's signing process. The server stores neither party's private key. Package source executes only under the consuming executable's grants.

`coven catalog --registry URL --operator OPERATOR_ID --registry-id ID --min-snapshot N` lists active release coordinates and their digests from one verified operator snapshot. It verifies snapshot freshness, then retrieves and verifies the exact index and owner history named by that snapshot. It includes at most 64 active releases across at most 64 admitted packages. Consumers that need package source use `coven fetch` to verify the release bytes separately.

## Read-only web UI

`coven serve --store STORE --port PORT --web-dir PROJECT/dist` also serves the
prebuilt Glamour catalog and its package detail pages. Coven validates and
loads the complete generation before binding the socket. It serves that
generation until restart. The `/v1/` contract and the private store are the
same with or without the web UI. [Build and serve the site](web/README.md)

## Who can reach the port

The server has no caller authentication. Any process of any local account can connect to the loopback port, read every public object and submit publications and snapshots. Integrity rests on signatures and on the operator: a publication is stored only when its owner signatures verify against the package's history, and it reaches clients only when the operator admits it into a signed snapshot, first claims by name. A local account can therefore claim unused names on the server and make the operator review them, but cannot make the operator sign them. A Unix-socket transport with peer-credential checks, so that only granted accounts can publish, is future work.

The server handles one request at a time. Each request, from its request line through its body, must arrive within 30 seconds in total, and the reply must be taken within another 30 seconds, so a client that sends or reads slowly holds the server for at most the respective deadline per connection. The reply worker receives only the staged reply bytes, bounded status and header metadata, and the accepted connection as its writable standard output. The registry API refuses requests with more or less than one `Host` header, an `Origin` header, or a JSON body that is not one flat object before publication or signing runs. Static web GETs accept an absent `Origin` or the exact loopback origin and require the same `Host`. Refusals name no server path: the store and staging directories, and any other absolute path, are replaced in the reply and logged in full on the server's standard error. The client escapes control and bidirectional-formatting characters in any text the server sends before showing it.

An HTTP publication keeps the owner's signed index aside (`$XDG_STATE_HOME/coven/signed/NAME-…pending`) until the server accepts it, and only then records it as the owner's copy. A refused publication therefore never becomes the base of the owner's next one. If the client stopped after the server accepted, the next publication finds the server serving exactly the pending bytes and records them then. A refused index's sequence is never signed again: the next index is numbered above it.

## Limits

This milestone serves one request at a time and admits JSON envelopes up to 16 MiB. Existing package record plus source limits still apply. A publication's `package` and `source` fields may together be a release of several files (`witchy-package 2`, see README "Layout"); the message format is unchanged. The server parses the release (canonical record, path rules, each file's length and digest, the import closure) before the owner, index and signature checks and refuses anything else, including a record in a format it does not know. A server from before format 2 refuses such an upload as an invalid record. A package publication stages the target package, the signed snapshot, and the named dependency digest markers. It can write only the standard objects in its version slot and package index. Package publication cannot delete registry objects. An operator snapshot or a local administrative command stages the full registry so it can review every package. Each mutation stages its files privately, streams SQLite BLOBs in bounded chunks, and commits atomically. The server refuses nonbinary objects, noncanonical paths, and nonregular staged files. Use independent OS accounts where filesystem isolation between roles is required. Public Internet hosting, TLS termination and remote administrative key custody are outside this localhost release.

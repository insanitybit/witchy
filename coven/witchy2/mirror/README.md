# Coven mirror in Witchy

This service serves the public TUF tree produced by the repository publisher.
Its Witchy entry point receives one accepted connection and a read-only
capability for `published/`. It accepts `GET` and `HEAD` on canonical routes
for `metadata/timestamp.json`, versioned metadata, and SHA-384-prefixed targets.
`HEAD` checks the same representation and returns its headers without a body.
Both mirror transports require HTTP/1.1 and a slash-prefixed request target.
They accept at most 8 KiB of request headers and scan
only newly received bytes and the delimiter overlap when a header spans
multiple reads. They validate header lines and refuse body framing and
the Expect field. There are no directory listings. The timestamp uses `Cache-Control: no-store`.
Versioned files use immutable caching.

## Loopback verification

Place an already published tree in `published/`. Keep staged candidates and
keys outside that tree. Change the loopback port in `mirror.grants.toml` if
needed. Build, test, and launch through Grimoire:

```sh
grimoire build .
grimoire test .
grimoire run .
```

The service also runs directly under `witchy-wasmtime-host`, with the grant
flags shown below. Grimoire is only needed at build time for this launch.

```sh
witchy-wasmtime-host target/main.wasm \
  --allocator --no-links --listen=127.0.0.1:8418 --instance-per-connection \
  --dir="$PWD/published" --dir-rights=read \
  --deadline-ms=300000 --connection-deadline-ms=2000 --fuel=20000000 \
  --net-bytes=unlimited --net-bytes-per-connection=2113536 \
  --fs-bytes=unlimited --fs-bytes-per-connection=2105344
```

## HTTPS mirror

Use the same published tree for HTTPS. The TLS identity is read from
two files under the tree.

- `tls/authority` contains the external DNS name and port, such as
  `registry.example:443`, optionally followed by a newline. The name
  must match the certificate and client SNI. HTTP requests must name
  this authority, with the default port optional.
- `tls/chain` is a list of DER certificates, leaf first. Each DER value
  is preceded by its unsigned three-byte big-endian length. Client trust
  anchors are provisioned separately.

Keep the P-384 PKCS#8 DER private key outside the published tree. The
launcher supplies it through an inherited descriptor. The HTTP routes
serve canonical TUF metadata and target paths. Install a checked
TUF-backed Glamour generation to enable the catalog and package pages
on the same origin.

```sh
witchyc wasm-caps --policy witchy2/mirror_https.policy witchy2/mirror_https.wy
witchyc build witchy2/mirror_https.wy -o mirror-https.wasm \
  --heap-pages=9 --slots=64 --stack-bytes=16384
mirror_pin="sha384:$(shasum -a 384 mirror-https.wasm | cut -d' ' -f1)"
coven tuf serve --mirror witchy2/mirror/published --listen 127.0.0.1:8419 \
  --module mirror-https.wasm --module-pin "$mirror_pin" --tls-key /path/to/server-key.pkcs8.der
```

Supervised startup with `NOTIFY_SOCKET` requires the independently
provisioned PEM trust bundle and pinned identity-check module. The
[systemd deployment archive](../../deploy/witchy2/README.md#start-and-verify)
supplies the module and its pin. The deployed unit also requires the
independent SHA-384 pin of the PEM file. For a manual start, pass
`--tls-ca-file`, `--tls-ca-pin`, `--tls-check-module`, and
`--tls-check-pin`. Passing a CA pin requires an identity-check worker
that reports the digest of the actual PEM bytes it used.

```sh
coven web install ./site/dist --mirror ./witchy2/mirror/published
```

The install validates the complete generation before it replaces the route
table. The listener reads one bounded table and checks the SHA-384 of the
selected body before sending it. Catalog responses use the standard HTTP
writer for content length, type, and connection framing. The catalog's
CSP pins its bootstrap script. Package pages and the worker have separate policies. The public
routes expose only the catalog, its four assets, and release pages listed
in the active table. Deploy new catalog snapshots by repeating the install.
The publication operator owns this tree and its local route table.

The provided grants bind a loopback listener. The host starts a new
instance for each connection with a read-only tree and an opaque TLS
signer. It bounds time, instructions, and bytes. The host runs one
instance at a time and holds at most 16 waiting connections. Key-source
aliases are refused at file open.

For a supervised deployment, provision the prebuilt module and its
independent SHA-384 pin. The host checks the pin against the bytes it will
run. Under systemd, the launcher relays the host's readiness only after
the host binds its own listener. The launcher needs no compiler. See [the Linux deployment units](../../deploy/witchy2/README.md) for
public TCP ingress that keeps TLS in Witchy.

The client needs to support P-384 and `TLS_AES_256_GCM_SHA384`. If its
first ClientHello offers a different key share, the server requests
P-384 once and validates the second ClientHello before continuing.
Both mirror transports send a single `Content-Length` for successful metadata and package
responses. The length and body come from the same admitted file snapshot. The worker sends
the body in bounded chunks and retains the timestamp and immutable object cache policies.
Both transports check the admitted snapshot length before sending a success
header. A timestamp larger than 16 KiB or another object larger than 2 MiB
returns a bodyless, non-cacheable `503` if the host admitted the file.
Deploy a Wasmtime host that supports `fs_snapshot_remaining` with the mirror module.
Repository publication, CI staging, and maintainer approval use their
own authorities and entry points.

# Coven Web

Coven ships a read-only catalog and package release pages rendered by Glamour.
The catalog and its links work without JavaScript. With scripts enabled a
single Witchy Wasm worker can filter the catalog in place.

## Build and serve

For a signed TUF mirror that publishes [`glamour 0.3.0`](../../../grimoire/examples/glamour-catalog/glamour),
scaffold the site with the same independently provisioned root and release
trust used by Grimoire dependencies.

```sh
coven web init-tuf ./site --registry "$mirror_url" --id "$registry_id" \
  --root "$root_file" --root-pin "$root_pin" \
  --trust "$trust_file" --trust-pin "$trust_pin"
grimoire lock ./site --toolchain
grimoire lock ./site/browser --toolchain
grimoire review ./site
grimoire review ./site/browser
grimoire review ./site --write-policy
grimoire review ./site/browser --write-policy
grimoire web build ./site
coven web check ./site/dist
```

Use `--ca-file` on `init-tuf` for a private CA. The site includes up to
64 releases from one authenticated TUF snapshot. Both executable builds
must resolve their packages from that exact snapshot. A second catalog
verification checks for changes before the generation is published. Detail
pages identify the sealed target digest, which covers the release envelope.
Use Grimoire's package lock when you need the source digest.

Install the checked generation into the public tree used by the Witchy HTTPS
mirror. The HTTPS listener serves the site at `/` and keeps TUF metadata
and downloads on their existing routes.

```sh
coven web install ./site/dist --mirror "$published_tree"
```

Rebuild and repeat the install when the catalog should reflect a new TUF
snapshot. Under the publication lock, the installer checks the exact
timestamp and versioned snapshot recorded by the build. If the mirror has
advanced, rebuild against its current generation and retry. The install
replaces the route table only after every asset is present. The HTTPS mirror withholds the old catalog after the
timestamp changes. Package and metadata routes stay available. Each web
request checks the published timestamp and the selected body against one
route table. Previous content-addressed bodies stay available for requests
that already opened an earlier table. Build with the current Coven and
Grimoire before installing an older site on HTTPS. The loopback preview
also accepts previous generations through
`coven serve --web-dir ./site/dist`.

### Verify the installed build and browser

From the monorepo root, use the compiler, proof binding, host, and Python
fixture environment for the [Coven HTTPS witness](../tuf.md#verify-an-installed-client-against-the-https-mirror).

```sh
python3 coven/witchy2/tests/tuf_https_witness.py --web
python3 coven/witchy2/tests/tuf_https_witness.py --chromium "$chromium"
```

The first command seals and publishes the Glamour package, installs the
tools in a temporary prefix, and verifies the served HTML, worker, and CSP
over TLS with an independently provisioned fixture CA. The second command
also uses Node and the supplied Chromium executable to check the Wasm
filter, opaque iframe, browser storage and network denial, and Trusted Types.
Chromium uses a fresh profile and the fixture leaf key's pin for the test
connection.

Run the browser worker's module-limit checks with Node:

```sh
node --test coven/witchy2/tests/glamour_worker_limits.test.mjs
```

### Loopback snapshot registry

Install matching Coven and Grimoire releases, then start the loopback registry
and establish its operator. The Grimoire catalog walkthrough covers publishing
and admitting the `glamour 0.3.0` package. Set `url` to the loopback registry,
`operator` to its verified operator key ID, `publisher` to the verified Glamour
publisher key ID, and `registry_id` to the identifier signed in its snapshots.
Obtain both keys independently of the registry's replies.

```sh
coven web init ./site --registry "$url" --operator "$operator" \
  --registry-id "$registry_id" --publisher "$publisher"
grimoire lock ./site --toolchain
grimoire lock ./site/browser --toolchain
grimoire review ./site
grimoire review ./site/browser
# Inspect the drafts before writing them. If the policy differs from the
# authority you intend, keep your narrower policy.
grimoire review ./site --write-policy
grimoire review ./site/browser --write-policy
grimoire web build ./site
coven web check ./site/dist
```

Stop the loopback instance and start it again on the same port with the same
store and the opt-in web directory.

```sh
coven serve --store ./store --port 8787 --web-dir ./site/dist
```

Open `http://127.0.0.1:8787/`. Each row links to an active package version and
its release digest. This is the snapshot identified on the page. To show a
newer snapshot, rebuild the site and restart Coven after the build succeeds.
The old process keeps serving its frozen generation until restart. When
upgrading from a generation made before Trusted Types enforcement, rebuild
with the matching Grimoire before restarting. The build
publishes only after the selected snapshot still verifies and the site's
complete asset set passes the same loader used by the server.

Grimoire passes the renderer and browser-filter smoke test to Wasmtime on
inherited descriptors. The host verifies the compiled bytes against their
SHA-384 digests before admitting either module. The verified filter bytes
are also the bytes written to the generated site. Each guest has a lifeline
that closes when its launcher exits. Use Grimoire and Wasmtime from the
same client bundle.

The Witchy renderer reads each field into a bounded buffer before
validating UTF-8. The field limits are in bytes and cover the text before
its required LF delimiter. The catalog input may end after its headers
or a complete row. A missing field or delimiter, an oversized field, or
an invalid UTF-8 field yields no document.

## Threat model

Treat network replies, registry records and package source, catalog fields,
browser inputs, and guest output as untrusted. Treat the pinned operator and
publisher identities as authorization for the precise statements their keys
sign. A valid release still passes Grimoire's source digest and capability
checks. An admitted index authorizes displayed release metadata. It does not
imply that every listed package's source has been fetched or reviewed.

| Boundary | What enforces it |
|---|---|
| Registry to build | Grimoire verifies the operator through the same trust and handover rules used for its locked dependencies. Coven binds the catalog to those exact snapshot bytes and verifies the owner indexes and rotations. Grimoire independently locks, fetches and reviews the Glamour source and pins compiler and host bytes. A second catalog check aborts if the selected snapshot changes during the build. |
| Build to HTML | The guest gets bounded standard input, output and allocator, with four Wasm pages, per-page fuel and time limits, and an aggregate render deadline. The Witchy renderer returns a GDL1 document stream. Grimoire independently encodes the expected document from the verified catalog and checks the entire stream byte for byte. It generates HTML from the verified catalog using fixed elements, escaped text, and local links constructed from validated package coordinates. |
| Build to server | An atomic symlink swap publishes a complete generation. The serving operator chooses that directory. The loader holds one directory descriptor, rejects linked, group- or world-writable, or oversized assets and validates each digest and exact path. The loopback preview freezes verified bytes in memory. HTTPS verifies the current timestamp and each served asset. The local manifest catches incomplete and mixed output. It is a local receipt and its author is trusted. |
| Server to browser | Closed GET routes expose the index, exact version pages, and five fixed assets. The catalog bootstrap and stylesheet have integrity pins. The filter and its worker script are fetched as bytes and checked against SHA-384 pins before transfer. Detail pages load no script. The catalog allows one fixed sandbox document and denies worker startup in the parent page. |
| Sandbox to page | An iframe with `sandbox="allow-scripts"` runs at an opaque origin. Its response CSP permits the pinned inline broker and its blob worker. Its CSP denies fetch and ambient resource loading. The parent accepts one MessagePort from that frame and validates the complete visibility result before changing any row. |
| Worker to sandbox | The guest imports only the Witchy console ABI. It receives one bounded event and may return at most 65 bytes. Before compilation the worker admits only private memory32 with a fixed size of at most four pages total, within the Witchy section profile. A watchdog terminates the worker on timeout. The broker sends the result through the channel and never interprets it as HTML. |

The trusted computing base includes the installed Coven and Grimoire hosts,
their verification guests and Wasmtime host, the selected compiler and proof
binding, the site builder's account and supplied web directory, and the
browser engine and shipped host scripts. The browser trusts its loopback
origin. The renderer receives neither signing keys nor access to the registry.
Serving a completed generation needs no running compiler or proof tools.

## Browser-enforced injection boundary

On the Coven Web origin, HTTP responses enforce `require-trusted-types-for
'script'`. The catalog and detail pages deny policy creation. The sandbox
document has a private `glamour-worker` policy constrained to the one blob
worker URL it constructs from the pinned script bytes. The parent
accepts its MessagePort only from the expected iframe WindowProxy with
opaque origin. Messages carry the public module and worker bytes, then
the bounded filter events and visibility masks. The catalog remains
readable with filtering disabled in browsers without Trusted Types.
[HTML sandbox](https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-sandbox) · [CSP](https://www.w3.org/TR/CSP3/)

The browser limit covers declared WebAssembly linear memory. The worker
also rejects table and start sections, and requires the Witchy console
imports and expected exports. Wasmtime still performs its full Witchy
profile check during the build. Browser engine memory and compilation
overhead remain governed by the browser.
[WebAssembly binary module format](https://webassembly.github.io/spec/core/binary/modules.html)

This boundary applies to the covered browser injection sinks. Typed rendering
and complete output validation protect the initial HTML response. The browser
and same-origin code remain trusted. Keep unrelated content on a separate
origin and serve the site with the generated CSP headers.

This milestone supports 64 releases, one visibility island, ASCII search
queries and local read-only browsing. The frame is a fixed catalog-filter profile. A general extension interface
for independently deployed script applications remains open. Validation
used loopback preview or HTTPS test deployment and Chromium with fixture
trust. Public registry writes and TUF have separate release gates.

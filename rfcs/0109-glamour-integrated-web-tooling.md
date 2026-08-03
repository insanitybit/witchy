---
rfc: 0109
title: "Glamour integrated web tooling and safe hot swap"
status: proposed
created: 2026-07-29
superseded-by:
tracking: "RFC-0107 Phase 4"
predecessors:
  - "[0107](0107-glamour-next-generation-web-framework.md) (Glamour 1.0 delivery contract)"
  - "[0108](0108-glamour-stateful-browser-abi.md) (stateful browser ABI)"
---

# RFC-0109: Glamour integrated web tooling and safe hot swap

## Summary

Witchy ships its maintained web workflow in the `witchy` executable:

```text
witchy new --web <directory>
witchy dev [--host 127.0.0.1] [--port 3000] [directory]
witchy test --web [directory]
witchy build --web [--out dist] [directory]
witchy doctor --web [directory]
```

The workflow requires no Node, npm, package-manager plugin, or third-party
development server. The compiler builds the application and static plans; a
Rust host writes deterministic artifacts, serves loopback development traffic,
watches source inputs, and publishes bounded diagnostics and reload decisions.

Development builds may expose authenticated debug metadata and model migration
adapters. Production builds omit them and emit a machine-readable report proving
their absence.

## Command routing

The native dispatcher recognizes the exact `--web` forms before forwarding
`new` and `build` to the embedded package manager and before parsing ordinary
`witchy test` options:

- `new --web` creates a web project and refuses a nonempty destination;
- `build --web` performs one production build;
- `test --web` builds in test mode and runs the browser-independent conformance
  suite, with real-browser selection added by an explicit option;
- `dev` is web-only in Phase 4 and starts the integrated development loop;
- `doctor --web` is read-only and reports every prerequisite and policy check.

Unknown flags are errors. No web flag silently falls through to another
frontend.

## Project contract

A generated project contains:

```text
witchy.toml
src/main.witchy
web/index.html
web/public/
```

`witchy.toml` names the browser entry and app identity. The entry exports or
generates the RFC-0108 source family. The generated starter is formatted,
capability-empty apart from its `UiRoot`, and passes `witchy test --web`
immediately after creation.

Production host ports are selected only from the toolchain-owned registry:

```toml
[web.ports.passkeyLogin]
adapter = "credential.get-exchange.v1"
endpoint = "/api/passkeys/login"

[web.ports.passkeyRegister]
adapter = "credential.create-exchange.v1"
endpoint = "/api/passkeys/register"
```

The table maps the public name used by `glamour.credential_port` to one
versioned built-in adapter and one canonical same-origin POST endpoint. The 1.0
registry contains only host-custodied WebAuthn authentication and registration
exchanges. The browser host posts the credential response directly and returns
only `{ok, status}` to Witchy. Unknown adapters, malformed adapter records,
configured-but-unreachable entries, reachable-but-unconfigured ports, and
secret submissions routed to these adapters fail production checking. A file or
package path is never a production adapter. The selected adapter identity and
endpoint, its fixed 60 KiB request limit, and its fixed 128-byte result limit
enter the descriptor policy, artifact grant, build identity, route authority
summary, and browser policy.

Static delivery also selects a closed hosting profile:

```toml
[web]
hosting = "portable" # or "headers-required"
```

`portable` is the default and records response-header-only enforcement as
degraded while retaining meta CSP and `_headers`. `headers-required` marks every
emitted route header as a deployment requirement. `witchy doctor --web --url`
must compare deployed headers with the manifest before such a deployment may be
promoted.

Runnable output also selects one RFC-0013 grant document with
`web.grants = "web/grants.toml"`. That document contains exactly one
`[user_caps]` entry of type `UiRoot`, with the single public string field
`policy`. Client delivery always requires it; static delivery requires it when
the checked site contains an interactive region. Omission, multiple roots,
additional host-capability sections, unknown fields, and non-string policy data
fail before executable compilation. A zero-runtime static site may omit the
file because it mounts no browser authority.

The tool canonicalizes the selected root as a closed
`witchy.web.ui-root-grant.v1` record. Its SHA-256 digest participates in the
production build and executable identities. Generated projects include the
explicit grant file; no application-name or ambient-host fallback acts as an
implicit grant.

The production Wasm repeats the canonical binding in one
`witchy.web.mount-grant` custom section. The maintained loader authenticates
that section against the manifest before instantiation and then supplies its
single policy field through the ordinary grantable-capability host ABI.

The tool owns `.witchy/web/` as a local cache. It never writes generated state
into `src/` and never requires the cache to reproduce a production build.

## Production artifact

`witchy build --web` writes through a fresh sibling staging directory and
atomically replaces the requested output only after every artifact validates.
The output is:

```text
index.html
assets/app-<content-id>.wasm
assets/witchy-runtime-<content-id>.mjs
assets/glamour-runtime-<content-id>.mjs
assets/app-<content-id>.css
witchy-web-manifest.json
witchy-build-report.json
witchy-sbom.cdx.json
_headers
```

Empty CSS is omitted. Every filename identity is derived from final bytes.
`index.html` references only manifest-listed assets. The manifest records
protocol version, application/build identity, template and sink tables, route
base, artifact hashes, and production/debug feature bits.

The report records:

- compiler version and embedded commit;
- source and dependency identities;
- complete runtime/build capability summaries;
- generated headers and deployment assumptions;
- artifact sizes and hashes;
- source-map policy;
- absence of development exports, traces, overlay code, and dev authority.

The CycloneDX SBOM records the Witchy package graph and generated runtime
components. It contains no source text, environment values, credentials, or
absolute local paths.

`_headers` supplies the minimum safe defaults needed by the emitted features.
Optional cross-origin isolation is enabled only when a selected feature
requires it; ordinary Glamour output does not impose it.

## Development server

`witchy dev` binds to `127.0.0.1` by default. Binding another interface requires
an explicit `--host` and prints the exposed address. It serves only the project
artifact graph and two development endpoints:

- `GET /__witchy/events` is a server-sent event stream;
- `GET /__witchy/diagnostics/<generation>` returns one bounded JSON diagnostic
  document.

The server provides no directory listing, file read proxy, shell endpoint,
compiler RPC accepting source text, or ambient credential bridge. Paths are
normalized and must resolve inside the generated artifact root. Requests use
strict method, body, header, and concurrency limits.

The watcher computes a dependency graph from compiler inputs. A changed source,
template, CSS, route, or public asset invalidates only its dependent compilation
units. Cache keys include compiler identity, package graph, target, mode,
normalized source, generated metadata, and relevant public assets.

## Reload protocol

Each successful development build publishes:

```json
{
  "generation": 42,
  "buildId": "authenticated build identity",
  "modelSchema": "authenticated concrete model schema identity",
  "templateSchema": "authenticated template/sink identity",
  "assets": ["changed content identities"],
  "decision": "swap | reload"
}
```

The browser accepts generations monotonically and fetches only manifest-named
same-origin assets. A failed build leaves the last good application running and
shows its diagnostic; it never swaps a partial artifact set.

State is preserved only when all of these hold:

- old and new builds explicitly contain the development migration ABI;
- application identity matches;
- concrete model schema identity matches, or a generated typed migration names
  the exact old and new identities;
- authorization shape and granted host descriptors match;
- protocol major and persistent-state format match;
- snapshot and restore stay within manifest limits.

Otherwise the server emits `reload` with a human-readable reason. The browser
must not infer compatibility from field names, JSON shape, source filenames, or
successful deserialization.

Development adapters are compiler-synthesized:

```text
__glamour_dev_snapshot() -> borrowed bytes
__glamour_dev_snapshot_length() -> i32
__glamour_dev_restore(pointer, length) -> output pointer
__glamour_dev_metadata() -> borrowed bytes
```

`__glamour_dev_metadata` returns a pointer to a compiler-owned immutable
length-prefixed record: a little-endian `u32` payload length followed by the
payload. `__glamour_dev_snapshot` returns a pointer to the first snapshot
payload byte, and `__glamour_dev_snapshot_length` returns its exact length.
The host copies that payload into memory obtained from
`__glamour_input_reserve` before calling restore.

The version-one snapshot payload is canonical little-endian data:

```text
"WGST" | u16 format | u16 field_count | 32-byte model_schema | 8-byte slots...
```

Every slot has a compiler-declared scalar kind and canonical eight-byte
representation. Metadata names the format, model schema, authorization schema,
protocol major, slot kinds, and hard byte limit. Future aggregate formats must
receive a new format number and deterministic checked serializer; they may not
copy process-relative pointers or infer fields from source names. A compiler
that cannot serialize and restore the complete public model exactly omits the
migration exports, records that reason in development metadata, and causes a
normal reload.

Restore validates the magic, format, length, field count, model schema, and slot
kinds before changing instance state. It either installs the complete model and
returns its first output or leaves the new instance uninitialized. Snapshot is
available only while the old instance is live, idle, and has no borrowed output.

The snapshot contains public application state only. Capabilities, authority
tokens, secrets, pending host work, DOM nodes, function values, and opaque host
handles cannot enter it. The old host cancels work and disposes only after the
new instance validates and restores successfully. A restore failure keeps the
old instance live and reports a bounded diagnostic.

Production compilation rejects or strips the development family before export
synthesis.

## Diagnostics and source mapping

Compiler metadata maps templates, holes, CSS rules, event plans, routes, binary
operations, and generated declarations to authenticated Witchy source origins.
The development build emits a private mapping artifact addressed by build
identity. Production maps are opt-in and never contain absolute paths or source
contents unless explicitly requested.

The browser overlay renders inert text in a closed shadow root. Diagnostics are
bounded and structured: generation, phase, severity, code, message, relative
source, span, expansion trace, and related locations. Untrusted source,
application payloads, host errors, and secrets are never assigned to HTML sinks.

Runtime diagnostics identify the last accepted event and output sequence,
template/operation identity, and mapped Witchy location without echoing payload
bytes.

## Developer tools

Development builds expose a read-only bridge with:

- current build/generation and lifecycle state;
- normalized template and route identities;
- bounded event/update/effect timing;
- active effect/subscription counts and stable public IDs;
- capability and host-descriptor summaries;
- optional redacted public-model snapshots.

The bridge cannot dispatch arbitrary messages, read secret controls, reveal
effect payloads, invoke host handlers, mint capabilities, or mutate the model.
Explicit test mode may enable authenticated scripted dispatch on a separate
build identity.

## `doctor --web`

The doctor verifies without changing the project:

- project and entry resolution;
- compiler/browser protocol compatibility;
- target availability and capability footprint;
- production and development export policies;
- template/CSS/route/accessibility manifests;
- deterministic rebuild identities;
- output path writability without overwriting it;
- loopback bind availability when requested;
- browser-test availability when selected;
- headers, SBOM, and report consistency.

Human output gives one remediation per failure. `--format json` emits a stable
schema and uses exit 0 for pass, 1 for failed checks, and 2 for invalid usage.

## Testing and exit evidence

Phase 4 requires:

- clean-directory `new -> test -> build` without Node/npm;
- deterministic byte-for-byte rebuild;
- staging failure preserving the previous output;
- source, template, CSS, route, and dependency invalidation tests;
- compatible swap, incompatible reload, failed restore, stale generation, and
  last-good-build tests;
- overlay XSS and diagnostic-bound tests;
- production export/import audit proving no development surface;
- report, SBOM, capability, hash, and header consistency tests;
- real release-browser smoke evidence tracked separately from local fake-DOM
  tests.

## Non-goals

Phase 4 does not add a plugin system, execute npm packages, preserve
incompatible state, expose a general browser REPL, or turn the development
server into a production server.

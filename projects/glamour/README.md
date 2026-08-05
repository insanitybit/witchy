# Glamour

Glamour is Witchy's experimental, capability-pure MVU UI substrate. It is not a
React clone and it is not production-ready browser infrastructure yet; it is the
smallest proof that a Witchy application can describe a UI without holding DOM,
network, storage, timer, or credential authority itself.

## Status

- **Status:** experimental prototype.
- **Authority goal:** empty capability footprint for the Glamour core and for
  applications that only compute `VNode` data and `Cmd` descriptions.
- **Trusted boundary:** the Witchy rune computes data; a host shell owns browser
  authority and interprets that data.
- **Primary implementation:** `src/glamour.witchy`.
- **Current examples:** [`examples/`](examples/) includes `counter`, `form`, `autocounter`,
  `examples/catalog`, `examples/package_page`, `examples/trust_view`,
  `examples/version_view`, `examples/coven_app`, `examples/coven_web_app`, and
  the RFC-0107 existing-syntax API prototype in `examples/next_api`.
- **Reference contract:** [`REFERENCE-PROTOCOL.md`](REFERENCE-PROTOCOL.md)
  specifies the current JSON host semantics.
- **Evidence:** [`RFC-0107-ACCEPTANCE.md`](RFC-0107-ACCEPTANCE.md) and
  [`PERFORMANCE.md`](PERFORMANCE.md) distinguish proven behavior from planned
  Glamour 1.0 work.

## Model

A Glamour application is ordinary Witchy data flow:

```text
view(state) -> VNode(msg)
update(state, msg) -> (state, Cmd(msg))
```

RFC-0107's next-generation path makes the complete state machine a value:

```text
Program(auth, model, msg)
Ui(msg)
Cmd(msg)
Sub(msg)
Resource(value, problem)
```

`Ui.map`, `Cmd.map`, and `Sub.map` compose child messages into parent messages.
`Program.initial` constructs state without authority, `Program.start` describes
authorized startup work for both fresh and resumed activation, typed event
decoders return allowlisted event data to Witchy, and stable command/subscription IDs provide replacement,
cancellation, stale-generation suppression, and deterministic host simulation.
HTTP, navigation, credential ports, and secret submission return typed
`HttpResult`, `NavigationResult`, or `PortResult` values through ordinary Witchy
callbacks; no result constructor is named by a string in the Program path.
This path currently extends the versioned JSON host; it does not yet claim the
binary-patch performance of the final RFC architecture.

Views may use either `html"..."` or `jsx"..."`. Both are ordinary Witchy
compile-time tagged literals backed by Glamour's checked parser; `jsx` is not a
language mode and does not bring React, hooks, or JavaScript expression syntax.
Both spellings emit a versioned static template plan with a semantic ID,
compiler invocation metadata, and typed dynamic-slot table. Dynamic URL holes
require a kinded `SafeUrl`; boolean, property, class, ARIA, and event positions
select distinct sinks.

Static styles use `css"..."`. The tag accepts sealed `css_asset` values only as
complete `background-image`, `border-image-source`, or `list-style-image`
values. Color, length, number, percentage, angle, and time properties accept
only their corresponding sealed `CssValue` category, including typed `css_var`
references. It rejects
direct URLs/variables, category mismatches, unsupported holes, and unsafe
stylesheet constructs. It scopes selectors to a deterministic sheet identity
and exposes declared classes through `CssClass`/`ClassList`. Deliberately
unscoped rules use the separate `global_css"..."` tag; publication records each
selector's tag invocation origin and attached routes in the build report.

Routes use a checked `RouteGraph`: normalized patterns receive semantic IDs,
dynamic parameters are named and percent-encoded, and the same declarations
support matching, URL construction, navigation commands, and static-path
discovery. Forms use `FormSchema` and typed field kinds for shared progressive
HTML facts and validation. Secret fields remain host-custodied. Client action
manifests receive compiler-owned input/result schema identities; project-authored
identities are rejected. Protocol 1.4 dispatches public form values and closed
completion statuses into Wasm while omitting secret values. Glamour derives the
same identities from `FormSchema` and decodes the frames into typed
`ClientActionInput` and `ClientActionCompletion` values, so application updates
need neither transport IDs nor JavaScript callbacks.

`button`, `decoded_button`, `image`, and `labelled_input` provide a first
native-semantics accessibility layer. The `html`/`jsx` compiler also rejects
statically knowable missing names, unresolved labels, missing image alternatives,
missing link destinations, and duplicate IDs.

Witchy target annotations (`@browser`, `@server`, and `@static`) prevent
cross-target calls and captured function references. `derive(PublicState)`
recursively proves that resumable/public state contains no capabilities,
functions, `Bytes`, secrets, or host handles. The proof trait is sealed against
handwritten and user-generated impls.

`VNode(msg)` is inert view data. Events carry typed `msg` values back to
`update`; they are not closures with ambient browser authority. Effects are also
represented as data through `Cmd(msg)`. The host shell decides whether and how to
perform those effects, then dispatches the resulting message back into the rune.

This split is the security property: the application can request navigation,
HTTP, timers, or host ports only by describing them. It does not receive `Net`,
`Clock`, DOM, cookies, WebAuthn, or storage capabilities.

The planned static production registry binds every reachable host port to a
versioned toolchain adapter:

```toml
[web.ports.passkeyLogin]
adapter = "credential.get-exchange.v1"
endpoint = "/api/passkeys/login"

[web.ports.passkeyRegister]
adapter = "credential.create-exchange.v1"
endpoint = "/api/passkeys/register"
```

This syntax is not yet accepted by production builds. The public names must
match literal `glamour.credential_port` policies once the host-custody exchange
lands.
Unconfigured, unused, unknown, and secret-custody-incompatible entries fail the
build. The adapter, same-origin exchange endpoint, and fixed request/result
limits enter the artifact grant. Credential responses are posted by the host and
never returned to Witchy; the application receives only HTTP success and status.
Routes derive `publickey-credentials-get` and
`publickey-credentials-create` Permissions Policy independently. Arbitrary
JavaScript module paths are not port adapters.

Static builds default to `[web].hosting = "portable"`, which works on GitHub
Pages with an audited meta CSP while reporting response-header-only protections
as degraded. Use `hosting = "headers-required"` when deployment must provide the
complete emitted CSP and Permissions Policy.

## Static sites

A static project sets `delivery = "static"` in its `[web]` table and exports one
ordinary, capability-free Witchy value:

```text
from glamour import Site

pub fn web() -> Site:
    glamour.site([
        glamour.static_page("/", home()),
        glamour.static_page("/about", about()),
    ])
```

Progressive forms use the same checked `FormSchema` in their HTML and site
manifest:

```text
pub fn web() -> Site:
    glamour.site_with_forms(
        [glamour.static_page("/", signup_page())],
        [signup_schema()],
    )
```

The build verifies each rendered form's schema identity, method, and safe
action URL against the manifest. Field kind and required facts are public;
secret field values never enter the site value or artifact graph.

`glamour.decode_form_entries` validates server or browser key/value pairs
against that same schema, including duplicate, unknown, required, and
field-kind checks. It separates ordinary values from server secret values. The
optional [`glamour_server`](../glamour-server/README.md) companion turns a
schema into a bounded, same-origin `std/server` action handler.
The browser host consumes the same checked fixture corpus, delegates submit
interception from the application root, and publishes explicit validating,
submitting, succeeded, failed, and cancelled states. It cancels superseded
requests, ignores stale completions, retains native behavior for cross-origin
or overridden forms, and keeps secret entries out of lifecycle values. Secret
fields require POST at the Witchy, compiler, and browser boundaries. Transport
from these public lifecycle values into typed Witchy application messages over
the optimized ABI remains later work.

Checked CSS and local preloads are part of the same typed site value:

```text
fn styles(logo: SafeUrl(AssetUrl)) -> CssSheet:
    match glamour.css_color_property("accent"):
        Err(_) -> css".card { display: grid; }"
        Ok(accent) ->
            match glamour.css_asset(logo):
                Ok(image) ->
                    css".card { color: ${glamour.css_var(accent)}; background-image: ${image}; }"
                Err(_) ->
                    css".card { color: ${glamour.css_var(accent)}; }"

pub fn web() -> Site:
    let logo = glamour.asset_url_or_empty("/logo.svg")
    let styles = styles(logo)
    glamour.site_with_assets(
        [glamour.static_page("/", home(styles))],
        [],
        [glamour.critical_stylesheet(styles, ["/"])],
        [glamour.static_asset(logo)],
        [
            glamour.static_preload(
                "/",
                logo,
                glamour.PreloadImage,
            ),
        ],
    )
```

Critical CSS is inlined only on its declared routes. Non-critical checked
sheets become deduplicated content-addressed assets. A preload must be local,
must name an emitted public file, and must match its declared style, font, or
image kind. The native build boundary recomputes CSS identities and rejects
forged constructors, unsafe or unscoped text, unknown routes, and missing
assets. A declared `StaticAsset` is read beneath `web/public`, emitted under a
content-addressed name, and substituted into matching checked HTML attributes
and preload records; typed CSS asset expressions are rewritten to that same
name. Undeclared, remote, non-canonical, direct, or forged CSS URLs fail. The
unhashed source-name copy is omitted.

Static custom-property assignments are built with matching generic categories:
`css_assign(color_property, color_value)` compiles, while assigning a length
value to that property does not. `css_custom_properties` emits only bounded
`--glamour-*` declarations, and native static publication reparses that closed
token grammar before allowing a `style` attribute. The optimized browser
protocol assigns numeric custom-property sink IDs and revalidates the sealed
value category before applying a dynamic `style.setProperty` patch.

`witchy test --web` evaluates the qualified `web()` declaration twice and
requires identical typed routes. `witchy build --web` renders each `Ui` with
Glamour's checked escaping rules, emits canonical route files plus manifest,
report, SBOM, headers, public assets, and optional content-addressed CSS, and
audits the result. A static build contains no JavaScript or Wasm; the audit
fails if either appears. `witchy doctor --web` reports route count,
capability-free evaluation, zero-runtime delivery, and determinism.
`witchy dev` serves every static route with last-good rebuilds and injects its
token-authenticated reload client into development responses only. Client-mode
development also fetches the exact build's private source map and exposes a
frozen read-only inspection snapshot: bounded operation identities and timings,
effect/subscription descriptor summaries, and model-field kinds whose values
remain redacted. The map includes ordinary Witchy function spans, emitted Wasm
function indices/body offsets, and compiler template-hole mappings. Production
bundles omit this bridge and the development Wasm metadata it requires.
Each mapped Wasm body also carries the exact absolute byte offset of every
decoded instruction boundary, without serializing operators or immediates.
Ordinary function metadata includes source `impl` methods; method bodies bind to
compiler-mangled Wasm names only when the exact or uniquely suffixed match is
unambiguous.
Compiler-owned development lowering also maps each source statement root to the
exact emitted instruction-ordinal and absolute byte interval it produced. The
sidecar is not embedded in production Wasm. A separate parser sidecar inventories
sorted, deduplicated nested expression-root ranges with exact line/column starts
and exclusive ends for loaded local functions and methods. It retains each
expression's parser-owned statement line and, when unambiguous, attaches that
statement's exact instruction and byte interval. The expression itself claims
no narrower byte interval: propagating expression identity through linking and
lowering remains work for the complete developer-tools contract.
Client-mode compiler invalidation follows the entry's loaded local import graph,
so editing an unrelated `.witchy` sibling does not trigger relinking; an imported
module edit does. Code generation still recompiles the complete linked unit.
The same 128-entry bound applies to asynchronous host lifecycle records. They
contain only effect/subscription kind, phase, numeric instance, descriptor,
generation, completion status, and an optional closed semantic category from
the authenticated descriptor (`resource`, `navigation`, `timer`, `port`,
`storage`, `worker`, or `custom`). Unknown categories are omitted. Requests and
results are never retained.
Codegen also exposes a one-byte-per-field change bitmap from inside Wasm.
Scalar fields compare directly; aggregate fields compare their compiler-known
bounded equality shape, including nested string and list content. Timeline entries
report only changed field indices, and inspection labels aggregate fields without
exposing their shape or value. Every model value remains in Wasm and displays as
`<redacted>`. Aggregate metadata uses tracing-only snapshot format zero and omits
snapshot/restore exports, so development rebuilds reload instead of copying raw
pointer arenas between modules.
Inspection also reports whether the current root was created by a fresh mount,
resume, or authenticated hot swap, plus live node/region/listener counts. It
does not expose DOM nodes or the dispatch-capable application object.
When explicitly enabled for development, the island scheduler separately
returns a fresh frozen hierarchy of checked instance/artifact/key identities,
parent identity, activation policy, inert/loading/active/failed/disposed status,
queued-event count, and authenticated resume-versus-fresh outcome. It omits
public-state JSON, DOM/application objects, event payloads, and dispatch handles.

## What is implemented now

- `VNode(msg)` element, text, keyed-node, structural-region, and compartment
  data constructors. `branch(id, active, node)` and
  `optional_child(id, template, Option(node))` give optimized islands stable
  compiler-authenticated branch and child regions; the current publication
  floor permits initially absent event-free branches and optional children
  without nested regions. The explicit optional-child template authenticates
  dormant structure without rendering a placeholder. Region plans retain
  authenticated following-sibling identities, so removal and re-entry preserve
  source order without wrapper elements.
- Identical compiler-generated island modules share one content-addressed Wasm
  executable while retaining separate instance, route, and activation records.
- The optimized host accepts protocol-1.1 scalar slot payloads for authenticated
  list, branch, and optional-child templates, validates exact slot coverage,
  and applies them before the detached subtree becomes live.
- Static island compilation assigns stable nonzero slot identities for text,
  property, attribute, boolean, typed-URL, class, ARIA, typed custom-property,
  and compatibility values, publishes the closed slot table, and emits exact
  re-entry payloads.
- Protocol 1.2 assigns numeric `--glamour-*` sinks and updates them only through
  the optimized host's authenticated `style.setProperty` boundary; generic
  dynamic style strings remain rejected.
- Attribute and event data constructors, including input-value events.
- `Cmd(msg)` descriptions for no-op, timer, batch, HTTP, navigation, and host
  ports.
- JSON serialization for the host-shell protocol.
- Typed `Site`/`StaticPage` declarations and deterministic zero-runtime static
  publication.
- HTML serialization used by static builds, tests, and examples to make escaping
  visible.
- Example applications that exercise counters, catalogs, trust/version/package
  views, and the Coven Web application shell.

## Not production-ready yet

- The browser host shell and build path still need routine, documented release
  verification before Glamour should be described as stable.
- The empty-footprint claim should remain a CI-enforced invariant for the core
  rune and flagship examples.
- Public docs should continue to label Glamour as experimental until the runtime
  boundary, CSP assumptions, deterministic demo data, and browser tests are all
  documented as a normal release gate.

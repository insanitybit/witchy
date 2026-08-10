---
rfc: 0107
title: "Glamour 1.0: capability-safe, compiler-directed web applications"
status: accepted
created: 2026-07-29
updated: 2026-08-10
superseded-by:
tracking: "Design accepted. RFC-0107 implementation is locally complete with all RFC-0107-owned acceptance rows `PROVEN`; public closure remains externally owned. As of the latest local revalidation pass, `node scripts/audit-browser-runnable.mjs` reports 150/150; `node scripts/validate_book_examples.mjs` reports 165 runnable / 59 non-runnable blocks and 0 divergence; 3-engine browser acceptance (`glamour-accessibility.spec.mjs`, `glamour-lifecycle-production.spec.mjs`, `glamour-islands-production.spec.mjs`) passes locally; local `./target/debug/witchy doctor --web projects/docs` and `./target/debug/witchy doctor --web --deployment <url>` checks pass. Latest local evidence run artifacts are in `projects/glamour/acceptance/doctor-gh-pages-2026-08-11.json` and `projects/glamour/acceptance/doctor-local-2026-08-11.json`. Remaining externally-owned evidence is public CI/header/hosting evidence, the approved WebAuthn relying-party exchange payload alignment, and cold-mobile matrix work."
predecessors:
  - "[0006](0006-compile-time-tagged-literals.md) (typed and hygienic `html` literals)"
  - "[0008](0008-frontend-framework-rune.md) (capability-pure model/update/view architecture)"
  - "[0015](0015-secure-web-by-construction.md) (secure web applications by construction)"
  - "[0039](0039-glamour-capability-safe-effects.md) (Glamour's grantable and narrowed UI authority)"
  - "[0041](0041-docs-as-a-glamour-app.md) (the Witchy book as a production Glamour application)"
  - "[0091](0091-browser-virtual-capabilities.md) (browser capability host)"
  - "[0102](0102-portable-roots-and-the-fetch-capability.md) (portable browser effects and Fetch)"
  - "[0103](0103-derived-platform-confinement.md) (derived CSP and browser confinement)"
related:
  - "[0100](0100-authorized-glamour-host-slots.md) (required authorization boundary for privileged host slots)"
  - "[0108](0108-glamour-stateful-browser-abi.md) (stateful Wasm application and checked binary browser protocol)"
---

# RFC-0107: Glamour 1.0 — capability-safe, compiler-directed web applications

> Provisional syntax. Code blocks in this RFC are intentionally not tagged
> `witchy`, so the runnable-documentation gate does not treat design sketches as
> implemented APIs.

## Summary

Glamour 1.0 will be Witchy's integrated web application system: an explicit,
deterministic state machine at the source level; a compiler-directed,
fine-grained DOM program at runtime; and a capability-confined host at every
effect boundary.

It will not copy React's component lifecycle or Hooks. Application behavior
will remain ordinary Witchy values and pure functions:

- `Model` is the complete application state;
- `Msg` describes a fact that happened;
- `update(auth, model, msg)` returns the next model and explicit commands;
- `view(model)` describes the interface;
- `subscriptions(auth, model)` describes ongoing external observations;
- the host performs commands and subscriptions only through granted
  capabilities.

The compiler will remove the costs and boilerplate that often make this model
feel less convenient than mutable component frameworks. Compile-time templates
will become stable DOM plans with typed dynamic slots. The model will stay in
Wasm memory. Browser events will enter through one delegated listener, updates
will execute in Wasm, and a compact checked patch stream will carry only
changed DOM values to the host. The current full-model/full-VNode JSON exchange
will remain temporarily as a compatibility path and then leave the production
path.

The same `Program` can be delivered in three modes without changing its state
semantics or acquiring a second rendering model:

1. **static** — native Witchy renders HTML and CSS; no Wasm or JavaScript is
   sent unless an interactive region is embedded;
2. **static with interactive regions** — application source embeds an ordinary
   `Program`; the compiler lowers each independently activatable region to a
   resumable Wasm island, or to an explicit fresh client region when no public
   model may cross the boundary, without whole-page hydration or a second
   `static_view`;
3. **client application** — one Glamour Wasm application owns a root and starts
   immediately, for dashboards, editors, and other interaction-heavy software.

`Program` is the authoring and feature-composition primitive. `Interactive` is
the small author-facing boundary for embedding a program in static content.
`IslandPlan` is the authenticated compiler/build representation of that
boundary and `island` is the runtime delivery term. Application authors do not
divide ordinary features into islands or manually coordinate island state.

The default project needs Witchy, a browser, and no Node installation. One
command starts the development server. The same command chain formats, checks,
tests, profiles, and produces a deployable directory.

Glamour 1.0 is complete only when it passes published criteria for:

- security and authority confinement;
- interpreter/compiled-Wasm semantic parity;
- DOM correctness and browser compatibility;
- accessibility;
- cold-start, update, memory, and bundle performance;
- error quality, feedback latency, and onboarding;
- migration of the Witchy book and representative applications.

This RFC is the start-to-finish delivery contract. It deliberately separates
the desired source language from the optimized implementation, so Glamour can
be more predictable than React, as concise as compiler-oriented frameworks,
and competitive with fine-grained runtimes without exposing a mutable reactive
graph to application code.

## Decision

Glamour adopts the following durable decisions:

1. **Explicit state transitions are the semantic core.** There are no
   call-order-sensitive Hooks, hidden component state cells, dependency arrays,
   or effect callbacks that run because a render happened.
2. **Commands and subscriptions are the only ordinary effect descriptions.**
   Constructing either requires the corresponding narrowed UI authority.
3. **Templates are compiled, not interpreted.** Static HTML structure,
   attribute kinds, event decoders, accessibility facts, and CSS are checked
   during compilation and lowered to a stable template plan.
4. **Fine-grained reactivity is an implementation property.** The compiler and
   runtime track changed template slots. Application authors continue to read
   normal immutable values, rather than manually creating or wiring signals.
5. **The browser host is small, security-critical, and defensive.** It does not
   trust application buffers: it validates a versioned patch protocol and
   applies an allowlisted set of DOM operations. It does not execute
   application strings, evaluate code, or receive the application's complete
   model.
6. **Static output is the default for static work.** Interactivity is explicit,
   independently loadable, and measurable.
7. **Programs compose behavior; interactive regions select delivery.** Child
   features compose through typed model/message/effect mapping. Embedding a
   program in static content creates an `Interactive` region that the compiler
   may realize as a resumable island. An island is not a component, state owner,
   or required application architecture.
8. **One view defines static and live output.** The normal program view is
   capability-free and is compiled for native static rendering and browser
   updates. A distinct fallback is allowed for a genuinely unavailable region,
   but a parallel full `static_view` is not part of the ordinary contract.
9. **Server rendering is portable.** A Glamour application can produce files or
   run behind any Witchy-capable host. It does not require a proprietary cloud
   protocol or a permanent application server.
10. **The safe path is the easiest path.** Raw HTML, arbitrary JavaScript,
   arbitrary DOM references, ambient browser globals, untyped URLs, and
   privileged host slots are not part of the default API.
11. **Performance claims require reproducible evidence.** Microbenchmarks,
   realistic application traces, Core Web Vitals, memory, and artifact size are
   all reported. No single benchmark can establish that Glamour is "fast."
12. **Glamour 1.0 is an integrated product.** Router, data loading, forms,
    styling, testing, accessibility, development server, diagnostics, and
    deployment output have one maintained path rather than a choose-your-own
    stack.
13. **Framework declarations are ordinary Witchy values.** Routes, sites,
    programs, and interactive regions begin as typed Glamour APIs built from
    functions, records, generics, traits, derives, and compile-time generation.
    `IslandPlan` is a compiler-authenticated lowering, not the primary authoring
    API. Glamour does not add an `island` keyword, built-in JSX grammar, or
    declaration annotation merely to shorten a library call. A `jsx`, `view`,
    or `html` compile-time tagged literal is an ordinary use of Witchy's
    existing general tagged-literal facility and is explicitly allowed.

## Motivation

### The present architecture proves the safety model

The implemented Glamour rune already establishes the right trust boundary:

- `view` and `update` are capability-pure;
- VNodes carry inert text, typed message values, keys, compartments,
  host-custodied secret fields, and presentation slots;
- commands describe timers, fetches, navigation, ports, and secret submission;
- narrowed `UiFetch`, `UiRoute`, `UiTimer`, `CredentialPort`, and secret
  capabilities make component authority reviewable;
- the DOM host rejects unsafe element names, attributes, and URL schemes;
- the host uses DOM construction APIs and has no HTML-string sink;
- derived CSP and opaque compartments provide a second browser boundary.

Those properties are a stronger foundation than frameworks that begin with
arbitrary JavaScript and attempt to recover safety through lint rules.

### The present transport is a prototype ceiling

The current browser loop is intentionally simple:

1. JavaScript serializes the complete model and message as JSON.
2. A `String -> String` Wasm export parses them.
3. Witchy runs `update`, recomputes the complete VNode tree, and serializes the
   model, VNode, and commands as JSON.
4. JavaScript parses the response, walks the old and new trees, patches the DOM,
   and retains the next model for the next event.

This is an excellent inspectable bootstrap protocol. It is not the final
architecture. Its work scales with model and view size even when one text node
changes. It allocates and parses strings at the Wasm/JavaScript boundary,
duplicates state outside Wasm, attaches per-node listeners, and makes the
JavaScript host responsible for general VNode reconciliation.

The current source API also lacks first-class composition, mapped child
messages, subscriptions, structured async state, typed event decoding, a
framework router, form validation, scoped styling, static/resumable rendering,
hot state-preserving reload, source-mapped browser diagnostics, and integrated
accessibility checks.

The next version must retain the security proof while replacing the prototype
costs and filling the product gaps.

### The market is asking for less ceremony and less machinery

The 2024 and 2025 State of JavaScript surveys repeatedly identify excessive
complexity, performance, state management, choice overload, breaking changes,
dependencies, bloat, and speed of change as front-end pain points. React, as
the dominant framework, receives the largest number of framework-specific
complaints. The 2024 meta-framework results separately call out complexity,
breaking changes, SSR, performance, documentation, deployment, and
front-end/back-end integration.

The positive signals point in a coherent direction:

- Solid has led State of JavaScript satisfaction for five consecutive years,
  and its fine-grained signal graph updates consumers rather than rerendering a
  component tree.
- Svelte compiles framework-aware syntax and makes granular updates feel like
  ordinary assignments.
- Vue's own comparison explains why automatically tracked dependencies and
  one-time setup avoid call-order restrictions, stale closures, dependency
  arrays, and routine callback memoization.
- Astro is static by default and sends client code only for explicitly
  interactive islands, proving the value of independently loadable content
  regions without making islands the right composition model for every app.
- Qwik demonstrates that listener identity, structure, and state can be made
  serializable so a page can resume instead of replaying every template.
- Marko demonstrates a stronger authoring direction: the compiler can infer
  static and interactive work below component boundaries and emit only the
  browser behavior that is required.
- Phoenix has been the most admired web framework in the Stack Overflow survey
  since 2023, evidence that an integrated model with explicit event handling
  and excellent tooling can matter more than ecosystem size.
- Elm demonstrates that model/update/view plus commands and subscriptions can
  make behavior inspectable, testable, and optimizable.

The shared lesson is not "adopt signals" or "adopt server components." It is:
make data flow obvious, let the compiler perform bookkeeping, send less code and
work to the browser, integrate the common path, and keep escape hatches visible.

### React's Hooks are not Glamour's template

React's official documentation describes Effects as an escape hatch and
teaches developers to remove many unnecessary Effects. Hooks have rules about
where and in what order they may be called. Effect correctness can depend on a
manually maintained dependency array and closure identity. React Compiler can
remove some manual memoization, but it adds another compiler analysis around a
runtime model whose lifecycle remains subtle.

Glamour already has a cleaner semantic answer:

- user interaction creates a typed `Msg`;
- `update` performs the corresponding state transition exactly once;
- external work is returned as a typed `Cmd`;
- ongoing external input is declared as `Sub`;
- derived view data is an ordinary pure calculation;
- the compiler can optimize the projection because effects cannot hide inside
  it.

We will improve the ergonomics of that answer instead of importing Hooks.

## Goals

Glamour 1.0 will:

- make small applications short and large applications structurally clear;
- preserve deterministic replay of messages and state transitions;
- make every external authority statically visible and dynamically confined;
- provide safe HTML, URL, CSS, event, form, and host-extension boundaries;
- update the DOM with work proportional to changed dynamic values;
- keep application state in Wasm;
- support keyed identity, focus, selection, scroll, and composition correctly;
- render static HTML and CSS without shipping an interactive runtime;
- let static pages embed ordinary programs as interactive regions;
- lower independently activatable regions to resumable Wasm islands, with an
  explicit fresh-start form for private browser-only state, without making
  islands the feature-composition model;
- provide client-only application roots when that is the correct delivery;
- require no Node/npm toolchain for the maintained path;
- provide an integrated router, resource model, forms, styling, testing, and
  accessible component foundation;
- produce useful diagnostics at Witchy source locations;
- expose enough telemetry to explain performance and authority use;
- pass a public conformance and benchmark suite on supported browsers;
- provide a compatibility path for current Glamour applications.

## Non-goals

Glamour 1.0 will not:

- implement React compatibility or reproduce the npm component ecosystem;
- expose Hooks, dependency arrays, hidden lifecycle effects, or mutable
  component-local state as its primary model;
- make arbitrary JavaScript execution or direct DOM mutation convenient;
- require a long-lived server for ordinary applications;
- require a particular hosting provider;
- promise Qwik-style per-function lazy loading in the first resumability
  release; the first lazy runtime unit is an independently compiled island;
- guarantee that every application state can be serialized;
- require authors to split ordinary applications or communicating features
  into islands;
- expose `IslandPlan` as the primary component or application API;
- treat server rendering as a substitute for authorization;
- optimize benchmark-only code paths that do not generalize;
- add a Glamour-specific `island` keyword, built-in JSX grammar, decorator
  system, or mutable reactive primitive;
- support obsolete browsers that lack the selected WebAssembly, module, CSP,
  and DOM baseline;
- freeze the 1.0 API before external pilot applications have exercised it.

## Design aesthetic

Glamour is a Witchy library, compiler path, and capability host, not a second
language grafted onto Witchy. Its source should look like the problem:

- domain state is represented by domain types;
- events are named facts;
- state changes are exhaustive matches;
- effects are returned at the point that decides they should happen;
- views read top to bottom as HTML with ordinary Witchy expressions;
- reusable features are modules with explicit inputs, state, messages, and
  authority;
- errors name the violated type, authority, or browser rule;
- production behavior can be explained from source without knowing compiler
  internals.

"Beautiful" does not mean minimizing line count at any cost. It means removing
incidental machinery while preserving the information needed to understand,
test, secure, and optimize the program. Glamour avoids:

- dependency arrays;
- memoization wrappers;
- lifecycle choreography;
- stringly typed routes, message tags, attributes, and CSS;
- framework-specific state containers around ordinary values;
- generated files that application authors must edit;
- mandatory configuration for the default path;
- a collection of interchangeable foundational packages that every project
  must select and integrate again.

The compiler may synthesize repetitive mapping, decoding, and patching code.
Tooling must show its expansion when that helps debugging. Concision never
hides authority or an effect.

## Witchy language and compiler requirements

Witchy already has the semantic power Glamour needs. Algebraic data types,
records, exhaustive matching, generics, traits, existential values, closures,
async functions, capabilities, user-defined derives, `comptime`, hygienic
quotation, and tagged literals are sufficient to express `Program`, `Model`,
`Msg`, `Cmd`, `Sub`, `Resource`, typed templates, CSS, routes, forms, and
interactive-region declarations. `IslandPlan` is generated from those ordinary
declarations rather than authored as a second application model.

Most of this RFC is therefore compiler, runtime, standard-library, CLI, and
tooling work rather than new language design. In particular, Wasm-resident
models, static template plans, binary DOM patches, event delegation,
incremental compilation, HMR, static rendering, artifact splitting, source
maps, and browser developer tools require no new application-facing syntax.

Three targeted general facilities may need to grow.

### Stable compile-time origins and metadata

Tagged literals can already emit hygienic typed AST. Glamour additionally
needs compiler-owned metadata to survive expansion:

- stable identities for templates, dynamic slots, events, CSS, routes,
  interactive regions, and their lowered island instances;
- original source spans for generated declarations and every literal hole;
- typed element, attribute, accessibility, and serialization facts;
- mappings from optimized Wasm and browser operations back to Witchy source.

IDs derive from authenticated declaration identity and structural position,
not filenames, byte offsets, generated names, or source formatting. The
facility is general compile-time infrastructure: `sql`, protocol generators,
and other DSLs can retain equivalent metadata without teaching the language a
Glamour-specific annotation.

The checked linker retains each expansion as a `TaggedLiteralOrigin`: a
compiler-owned ordinal, canonical definition-site tag, definition span,
invocation span, and exact hole-start spans. Checked web builds resolve the tag
through the loader-authenticated declaration catalog and attach its package,
version, module, and local declaration identity before publishing the private
development source map. The runtime AST and production Wasm contain none of
this authority or source metadata. Framework-specific template, sink, route,
and operation records build on this general inventory.

For Glamour, the compiler joins that inventory to checked `planned` expansions.
It authenticates the toolchain `TemplatePlan`, `html`, and `jsx` declarations,
validates each `glamour-tp1-*` identity and kinded slot table, requires exactly
one generated plan per retained tag invocation, and interns identical plans.
Source-line movement updates private spans without changing the semantic
template schema used for hot-swap compatibility. Stable nonzero template wire
IDs derive from the semantic digest rather than registry order; slot wire IDs
derive from checked hole indices. The registry also records the inert static
element/text skeleton, sorted static attributes, compiler-local node IDs, and
the exact node sink for every dynamic slot.

### Target availability and placement

Capabilities answer which external authority code can exercise. Glamour also
needs the compiler to prove where code and values may exist:

- static build;
- server;
- browser;
- shared across selected targets;
- public resumable state.

A target-unavailable call is a compile-time diagnostic at the reference site.
A server-only function cannot enter a browser artifact. A capability, secret,
host handle, function, stream, or unstable representation cannot enter public
state. Shared code must be available on every selected target.

This is availability checking, not unrestricted conditional compilation.
Absent code fails clearly; source does not silently acquire target-dependent
semantics.

### Checked generated boundary declarations

User-defined derives and structured compile-time generation provide the
starting point for `PublicState`, route codecs, form schemas, and host protocol
adapters. Their type information and diagnostics must be strong enough to:

- inspect nested nominal and generic shapes;
- reject forbidden values transitively;
- emit typed declarations without source-string round trips;
- preserve definition-site and call-site hygiene;
- attach the stable metadata above;
- show generated expansions through `witchy expand`.

`derive(PublicState)` is a checked proof that a value may cross one specific
boundary, not a general-purpose serialization promise. `PublicState` is sealed:
application code and user-defined generators cannot provide an unchecked impl.
Only canonical standard foundations and the compiler-authenticated recursive
derive may produce the proof.

### Syntax restraint

Phase 0 prototypes the site, route, `Program`, and low-level `IslandPlan`
machinery with existing Witchy syntax. Before Phase 6 freezes its public source
contract, the `Interactive` prototype must prove that the compiler can lower the
ordinary authoring value to that sealed plan without a second view function. A
new syntax proposal must identify an important contract that typed functions,
records, traits, derives, tagged literals, and `comptime` cannot express clearly.
Saving a few characters is insufficient.

This restraint does not reject a tag named `jsx`. For example,
`jsx"<Search model=${model.search}/>"` is the same general compile-time
mechanism as `html`, `sql`, or `css`: the tag parses its static fragments,
places hygienic Witchy `${...}` holes, and emits typed AST. It may support
HTML elements and call-site-resolved Witchy view functions without adding JSX
to the Witchy lexer or parser. JavaScript-style bare `{expr}` holes are not
needed; `${expr}` preserves the existing hygienic AST and source-span channel.

Associated types or module interfaces may eventually reduce generic noise in
large `Program` APIs. They are not Glamour 1.0 prerequisites and require their
own general-purpose evidence.

## User model

### A program is a state machine

The complete application contract is:

```
type Program(auth, model, msg):
    Program(
        authorize: fn(UiRoot) -> auth,
        initial: fn(Start) -> model,
        start: fn(auth, model) -> Cmd(msg),
        update: fn(auth, model, msg) -> (model, Cmd(msg)),
        view: fn(model) -> Ui(msg),
        subscriptions: fn(auth, model) -> Sub(msg),
    )
```

`Start` contains bounded public route and bootstrap information. It does not
contain ambient browser globals, authority, host handles, or model state. The
compiler owns its wire schema, and the browser adapter validates its protocol,
build, artifact, and instance identities before passing it to application code.
`initial` is the capability-free model constructor for a client root or fresh
client region. `start` describes the authorized startup work for an already
chosen model. A client root and a fresh client region call `initial(Start)` and
then evaluate `start(auth, model)`; a resumed interactive region uses its
authenticated public model and evaluates the same `start(auth, model)`. The host
does not perform the returned command until the initial DOM and private state
commit. This split prevents resumption from silently dropping startup work while
also preventing static rendering from acquiring authority.

The host supplies the application's declared `UiRoot`; `authorize` narrows it
once into an application-specific record whose fields are passed only to
`start`, `update`, and `subscriptions`. The record is runtime authority, not
model data: it is absent from model snapshots, static HTML, and resumable state.
An authority-free program uses an empty record.

`view` receives only the model. This makes the source projection deterministic,
lets native static rendering and browser updates compile the same function, and
prevents presentation from becoming an authority-acquisition path. Public facts
that affect presentation, such as `can_delete`, belong in the model.
Host-custodied secret controls remain inert typed template declarations whose
values never enter `view`; submission consumes the secret through an authorized
command. Presentation-only slots accept ordinary inert data. Before a
privileged slot ships, RFC-0100 must be revived with a compiler-authenticated
slot declaration and host grant that does not pass ambient authority into
application rendering.

The owned render value is named `Ui(msg)`, not `View(msg)`. Witchy already uses
`View(T, 'a)` for RFC-0083 borrowed values, and overloading that spelling with a
one-argument framework type would make ownership diagnostics ambiguous.
Application projection functions remain conventionally named `view`.

`update` is the only ordinary place an application changes its model. It may
return a batch of commands, but it does not execute them. `view` and
`subscriptions` are pure descriptions. The runtime compares subscription
identities after each update, starts new subscriptions, and cancels removed
ones.

### Features compose without hidden instances

A reusable feature is an ordinary module with its own `Model`, `Msg`, `initial`,
`start`, `update`, `view`, and optional `subscriptions`. Parent code maps child
messages and effects:

```
type Msg:
    Search(Search.Msg)
    Saved(Result(SaveError, Receipt))

fn view(model: Model) -> Ui(Msg):
    html"""
      <main>
        ${Search.view(model.search).map(Search)}
      </main>
    """

fn update(auth: Authority, model: Model, msg: Msg) -> (Model, Cmd(Msg)):
    match msg:
        Search(child_msg) ->
            let (search, cmd) = Search.update(auth.search, model.search, child_msg)
            (Model(..model, search: search), cmd.map(Search))
        Saved(result) -> ...
```

`Ui.map`, `Cmd.map`, and `Sub.map` are compiler-known adapters in optimized
output. `Ui.map` erases into the parent template/message lowering. `Cmd.map`
and `Sub.map` compose the mapper and any bounded captures into one generated
callback-table entry rather than allocating a chain of runtime callbacks. They
provide the composition benefit of components without creating a hidden
lifecycle or state owner.

The compiler may later derive the repetitive parent cases:

```
feature search: Search at model.search via Msg.Search
```

This is sugar for the explicit mapping above. The expanded form remains
inspectable in tooling.

### Interactive regions are delivery boundaries

Feature composition and delivery composition are different operations. A child
feature that shares parent state, messages, keyboard behavior, navigation, or
frequent updates composes through `Ui.map`, `Cmd.map`, and `Sub.map` inside one
`Program`. It is not an island.

Static content embeds a complete independently activatable program through the
sealed heterogeneous `Interactive` value:

```
pub fn interactive(
    app: Program(auth, model, msg),
    initial: model,
) -> Interactive where
    model: PublicState,
    model: Reflect,
    model: Deserialize
```

An embedded browser-only program whose initial model cannot cross the public
boundary uses the visibly distinct fresh-start form:

```
pub fn client_region(
    app: Program(auth, model, msg),
    fallback: StaticUi,
) -> Interactive
```

`StaticUi` is a sealed event-free static template value. The compiler rejects
event bindings, browser-only slots, commands, and capabilities in it. On
activation, `client_region` validates a fresh-start input, calls `authorize`,
constructs the model with `initial(Start)`, derives and validates the live view,
and evaluates `start(auth, model)` plus initial subscriptions. It then replaces
the fallback and installs the live event registry in one controlled commit.
Only after that commit may the host perform the startup command or start a
subscription. The activation is reported as `fresh`, never `resumed`. This form
exists for maps, editors, canvas integrations, and state that correctly starts
only in the browser; it is not a second full rendering function.

The closed publication record carries an explicit `resume` or `fresh` mode.
Only `resume` admits a canonical public-state frame; `fresh` requires the state
field to be absent/null and carries checked `Start` data separately in its
activation input. The loader dispatches a fresh record only to the artifact's
compiler-owned fresh entry and never attempts resume or mismatch recovery for
it.

The ordinary form is intentionally short:

```
glamour.interactive(Search.program(), initial)
```

The resumable form uses the program's existing `view(initial)` for static HTML.
There is no parallel full `static_view`.

Advanced delivery controls remain ordinary builder methods:

```
glamour.interactive(Search.program(), initial)
    .named("search")
    .activate(glamour.OnInteraction)
    .prefetch(glamour.PrefetchVisible)
```

The default activation policy is `OnVisible`, so visible controls begin
activation before likely input. `OnInteraction` is an explicit size/startup
tradeoff, not the default user-experience promise. Prefetch may fetch and cache
content-addressed code, but it cannot instantiate the program, mint authority,
start subscriptions, or execute application logic.

The delivery controls are closed framework values:

```
type Activation:
    OnLoad
    OnIdle
    OnVisible
    OnMedia(MediaQuery)
    OnInteraction

type Prefetch:
    NoPrefetch
    PrefetchIdle
    PrefetchVisible
    PrefetchMedia(MediaQuery)
    PrefetchIntent
```

`Interactive` defaults to `OnVisible` and `NoPrefetch`. Prefetch and activation
are separate so downloading immutable bytes never silently creates authority or
starts application work. `MediaQuery` is a sealed value from the checked CSS
media-condition parser, not an arbitrary runtime string.

Application source obtains a media condition through a static, hole-free
`media"..."` tagged literal or a named framework constant. There is no public
`String -> MediaQuery` constructor. The literal, CSS `@media` parsing,
publication, and the browser manifest all use one normalized grammar and test
corpus; a value that one layer cannot reproduce exactly fails the build.

`OnVisible` activates only when the region intersects the viewport.
`PrefetchVisible` starts immutable-byte retrieval when the region enters a
one-viewport lookahead margin; it does not satisfy or trigger `OnVisible`.
`OnMedia` and `PrefetchMedia` independently observe the same normalized sealed
condition through `matchMedia`, while CSS emission uses that exact parsed
condition. A parser or normalization disagreement is a build error.

`Interactive` enters static content through one checked placement operation:

```
glamour.embed(glamour.interactive(Search.program(), initial))
```

`embed` returns a boundary node in the surrounding static `Ui`; the embedded
program's message type cannot escape into the surrounding page. A checked
`html` or `jsx` hole may expand to the same operation. `Interactive` cannot be
stored in attributes, text, model state, commands, or ordinary child-feature
composition.

The compiler authenticates the direct toolchain `interactive` or
`client_region` constructor, its direct zero-argument program factory with an
explicit `Program(auth, model, msg)` result, every builder operation, and the
checked `embed` placement. It captures program declaration identity, concrete
auth/model/message identities, view identity, and codec identity out of band.
The evaluated `Interactive` value contains only inert policy, diagnostic,
static-render, and public-state data. Indirect program selection, forged
constructors, escaped values, and placements without authenticated provenance
fail publication.

This provenance join is identity-based, never traversal-order-based. The
compiler derives a constructor origin from the authenticated containing
declaration and its structural `Interactive`-call ordinal, and derives a
distinct placement origin from the authenticated `embed` tree path. Formatting,
source-line movement, diagnostic names, evaluation order, unrelated expressions,
and registry sorting do not affect either identity. Reordering the constructor
calls or placement tree intentionally changes the corresponding structural
identity. A compiler-private opaque origin token may survive static evaluation
inside the sealed value solely to authenticate this join; it is not
source-constructible, is stripped before public manifests and Wasm, and does not
enter the executable artifact identity. Reusing one authenticated constructor
at several `embed` placements produces several isolated instances that may
share one content-addressed executable.

The compiler then lowers `Interactive` to a sealed `IslandPlan` in the private
build graph. `IslandPlan` is visible in build reports and advanced tooling, but
it is not the application composition type. Multiple regions that need shared
mutable state must become one program or communicate through an explicit
external protocol; Glamour does not create an implicit cross-island store.

### Effects are typed events, not lifecycle reactions

Commands represent finite work:

- fetch a typed request;
- navigate;
- schedule a timer;
- invoke an authorized host port;
- persist through a granted storage provider;
- submit a host-custodied secret;
- run a worker task;
- focus or measure an owned element through a narrow UI capability.

Subscriptions represent ongoing external input:

- route changes;
- animation frames;
- intervals;
- viewport or media-query changes;
- online status;
- authorized host streams;
- compartment messages.

Every effect-bearing command and every subscription source has:

- a compiler-authenticated descriptor identity;
- narrowed authority at construction time;
- a typed result-message adapter;
- a cancellation policy and, where applicable, a runtime cancellation or
  synchronization identity;
- an authenticated owner declaration and live owner instance;
- a redacted debug representation.

The optimized compiler closure-converts every reachable result-message adapter
into a closed callback table. An entry contains a compiler-owned ordinal, the
typed result schema, the final `Msg` type, and a bounded private environment
shape for captured ordinary values. The callback portion of persistent
application state stores only that ordinal and a deep-copied environment; it
never stores a Wasm-GC closure or publishes a function value. Capture-free
variant constructors have an empty environment. `Cmd.map` and `Sub.map`
closure-convert the composed mapper and its bounded captures into one generated
ordinal rather than allocating another runtime callback.

An adapter is eligible for optimized output only when the compiler can prove the
callback target and every capture transitively. Capabilities remain in the
private authorization record and are not callback captures. Host handles,
streams, secrets, DOM values, arbitrary dynamically selected functions, and
captures without a bounded persistent representation fail at the source callback
with guidance to use a named typed message adapter. The JSON compatibility path
may retain its existing typed callback encoding, but production optimized output
does not use callback probes or infer a message variant by comparing serialized
values.

“Bounded” is a release-enforced resource contract, not only a known type shape.
The artifact manifest caps one environment's encoded bytes and nesting, the
number of pending one-shot effects and live subscriptions, and their aggregate
private bytes. String, byte, list, dictionary, and nested aggregate captures are
checked against those limits before a work record becomes visible to the host.
Exceeding a limit fails that command construction with a source-attributed
diagnostic; it never starts host work and never truncates a capture. Development
tooling warns when an adapter snapshots a large aggregate such as the complete
model when a smaller message constructor would preserve the same semantics.

Each compiler-owned effect or subscription descriptor names the callback
ordinal, request and result schemas, owner-scope declaration, cancellation
policy, and source origin. Runtime work records separately carry the effect
instance and optional cancellation key or the stable subscription identity,
plus the numeric descriptor, live owner instance, and checked request data. A
runtime string selects neither an owner nor a message adapter.

On completion, the host validates application, build, descriptor, live work
identity, host-owned generation, and result schema before copying one bounded
completion frame into Wasm. Witchy independently validates the frame identity
and sequence plus its pending source, work identity, descriptor, result schema,
and callback entry. Input-frame sequence is independent of the output patch
sequence, so a completion cannot become invalid merely because an earlier
message changed no DOM slot or emitted only host work. Protocol 1 assigns
generations inside the host and does not acknowledge them to Wasm when work
starts, so Witchy requires a nonzero
generation and records it for diagnostics but does not claim an independent
generation-liveness proof. It then decodes the typed result, loads the private
callback environment, constructs the typed `Msg`, and appends it to the ordinary
FIFO queue.

The result schema selects a closed compiler/host codec. Production host adapters
encode only values admitted by that schema into a bounded canonical byte
payload; text is UTF-8 only in fields declared as text. Witchy validates the
complete payload and reconstructs the result type before invoking the callback
ordinal. Generic `String(value)`, callback probes, JSON-selected variants, and
application-supplied decoders are compatibility behavior, not an optimized
production completion path.

RFC-0108 pins the protocol-1 byte layout for the standard unit, HTTP,
navigation, and port/secret result schemas. The authenticated descriptor
semantic selects that fixed codec. A result-schema integer or application value
never selects code, and a generic UTF-8 fallback is forbidden for a production
descriptor.

An effect completion or cancellation consumes its callback environment exactly
once. A subscription emission does not: its environment remains live across
emissions and is consumed only when that subscription is removed, replaced, or
disposed. The pending registry makes a duplicate effect completion or an
emission after local teardown fail before message construction even if the host
boundary regresses.

Every effect and subscription has a compiler-authenticated owner scope. The
default is the application or island root. Route loaders, resources, and
subscriptions use the stable scope already generated for their declaration. A
view subtree may own work only through a checked structural scope whose
declaration and live template region share one compiler origin. Repeated keyed
or child regions therefore share a static owner declaration but receive
distinct numeric live owner instances; an arbitrary runtime string cannot name
either. `Cmd.map` and `Sub.map` preserve ownership. When a live scope leaves,
the runtime removes its private callback entries and cancels its subscriptions
and cancel-on-leave commands before accepting a later completion. An explicitly
detached command opts into root ownership and is reported as such by capability
and lifecycle tooling.

`Program.subscriptions` does not inherit the most recently handled browser
event. A subscription descriptor is its static owner declaration, and
reconciliation allocates a private numeric live owner instance for each
descriptor plus stable subscription identity. Two feature instances using the
same constructor therefore remain independent when their parent supplies
distinct stable identities; removing one subscription removes its callback
environment and owner instance without disturbing its sibling. This is
subscription lifecycle, not a hidden view-component lifecycle. `Sub.map`
preserves the descriptor and stable identity while changing only the final
typed callback.

The explicit source operation is `glamour.detach(command)`. It is deliberately
visible at the call site, composes through `Cmd.map`, and changes only the
ownership envelope: the command's typed result, capability grant, cancellation
identity, and callback remain unchanged. A detached command is not a fire-and-
forget escape hatch; disposal of the application root still cancels it.

The runtime message queue privately envelopes each typed `Msg` with its static
owner declaration and live owner instance. Browser events obtain that pair from
the authenticated event plan; effect and subscription completions inherit it
from the pending entry; startup messages use the root. Application `update`
receives only `Msg`, while compiler-specialized work emitted by that transition
inherits the envelope unless an authenticated route/resource constructor or an
explicit detach operation selects another scope. This causal envelope is how a
repeated structural region retains its distinct live owner without exposing an
author-forgeable owner token in application data.

The host-owned generation makes stale, replaced, and cancelled callbacks inert.
An application chooses race semantics before that boundary by using one stable
replacement identity, distinct concurrent identities, or explicit model-level
request IDs; a callback from a stale host generation never enters `update`.

### Resources make async state complete

Glamour provides a typed resource state:

```
type Resource(value, problem):
    Idle
    Loading(RequestId, Option(value))
    Ready(value)
    Failed(problem, Option(value))
```

Router loaders and form actions use this type by default. The generated update
path handles request identity, cancellation, stale completion, and optional
stale-while-revalidate behavior. Applications can use raw commands when they
need a different state machine.

There is no hidden fetch triggered by reading a value during rendering.

## Template and view design

### `html` becomes a typed static template plan

RFC-0006's hygienic `html` literal becomes the preferred view syntax. At
compile time, the literal is split into:

- an immutable DOM skeleton;
- typed text slots;
- typed attribute/property slots;
- conditional regions;
- keyed repeat regions;
- child-view regions;
- event decoder and message constructors;
- interactive-region mounting points;
- static accessibility facts;
- static CSS references.

Glamour exposes `html` and `jsx` as library-defined compile-time tagged
literals. Phase 1 keeps `jsx` as a parity-checked alias over the existing safe
`html` parser. Phase 2 grows that shared parser into a static template planner;
`jsx` may then include both platform elements and capitalized Witchy view
functions. The spelling does not imply a JavaScript-compatible grammar built
into Witchy, and `${expr}` remains Witchy's hygienic hole syntax.

The compiler assigns stable template and slot IDs derived from semantic source
identity, not byte offsets. Formatting a file does not invalidate every ID.

A text-position hole accepts only `String`, `VNode(msg)`, or `Ui(msg)`.
Compiler-directed lowering turns `String` into inert text and splices checked
`VNode`/`Ui` structure through a stable child region. Other values fail with a
focused type diagnostic; they do not gain an implicit markup or display
conversion. `glamour.embed(interactive)` returns `VNode(Nil)`, explicitly
erasing the island's private message type at this delivery boundary so
heterogeneous independent regions can share one static parent template.

CSS and delivery policies share the sealed `MediaQuery` grammar. Scoped and
global styles accept only top-level `@media` blocks; scoped sheets apply their
selector scope recursively inside those blocks, while global sheets preserve
normalized selectors. Both reject nested or unrelated at-rules. The
compiler, native publisher, and browser loader use one bounded disagreement
corpus for byte length, allowed characters, and balanced parentheses.

For example:

```
fn view(model: Model) -> Ui(Msg):
    html"""
      <form on:submit=${event.prevent_default(Save)}>
        <label for="name">Name</label>
        <input
          id="name"
          name="name"
          value=${model.name}
          on:input=${event.value(NameChanged)}
        >
        <button type="submit" disabled=${model.saving}>
          ${if model.saving then "Saving…" else "Save"}
        </button>
      </form>
    """
```

Static element and attribute names are checked against a pinned browser schema.
Dynamic tag names and arbitrary attribute names are not accepted by this safe
form. The builder API remains for genuinely dynamic structure, with the same
safe types and a slower general reconciliation path.

### Text and markup stay different types

Interpolated strings are text. They cannot become markup.

Trusted static markup originates only from a checked `html` literal. A future
sanitized-markup API returns an opaque `SanitizedHtml` value from a named,
audited sanitizer policy and requires an explicit sink. There is no
`String -> Html` conversion.

The DOM host never uses application-provided `innerHTML`. Static skeletons may
use browser `template` cloning only when the bytes were compiler-emitted and
bound to the build manifest; a strict Trusted Types policy owns that sink.

### Attributes, properties, and URLs are distinct

The current generic `Prop(String, String)` shape becomes a compatibility API.
The typed template distinguishes:

- text attributes;
- boolean attributes;
- enumerated attributes;
- DOM properties such as input value;
- token lists such as classes;
- ARIA attributes;
- event bindings;
- URL-bearing attributes;
- style values.

URL slots require a `SafeUrl(kind)` value. Constructors parse and normalize the
URL, enforce allowed schemes, and preserve whether a value is navigational,
image, media, download, or Fetch authority. A plain string cannot reach an
`href`, `src`, `action`, or equivalent URL sink.

### Events use declarative typed decoders

Event handlers carry a typed message value or a restricted decoder:

```
on:click=${event.msg(Increment)}
on:input=${event.value(NameChanged)}
on:keydown=${event.key.filter(key.Enter).map(Submit)}
```

Decoders are data. They may read an allowlisted set of event fields and compose
with `map`, `filter`, `prevent_default`, and `stop_propagation`. They cannot
capture a DOM object or execute arbitrary host code.

One delegated listener per event class is installed at the application or
island root. Stable event IDs map directly to decoder plans and message
constructors. Events from secret fields continue to expose only
non-sensitive status values.

### Lists have explicit identity

Repeated stateful content requires keys:

```
${view.each(model.todos, key: fn(todo) -> todo.id, render: todo_view)}
```

The compiler warns when a dynamic list contains inputs, focusable nodes,
transitions, or child features without keys. The runtime uses a move-minimizing
keyed algorithm and preserves node identity, focus, selection, form state, and
scroll position.

An unkeyed list is allowed for short, stateless content. Its behavior is
positional and documented.

### Host failure boundaries are explicit

Failures are ordinary variants in models and resources. View-level boundaries
handle only rendering failures originating in framework-owned adapters or
privileged host extensions:

```
view.boundary(
    body: risky_host_view,
    fallback: fn(problem) -> error_view(problem),
)
```

The boundary does not catch capability denials and silently continue. Authority
failures return through the command's typed result or terminate the affected
host extension according to policy.

## Compiler-directed updates

### Template plans, not a runtime VDOM, are the fast path

Each compiled template contains:

- a stable template ID;
- static DOM construction data;
- a table of dynamic slots and their value types;
- a table of event plans;
- region metadata for branches and keyed lists;
- source mappings;
- accessibility metadata;
- resumability metadata.

On initial client mount, the Wasm program emits a `Mount(template_id, slots)`
record. The host clones or constructs the known skeleton and fills the slots.

After `update`, the Wasm runtime evaluates the view and compares dynamic slot
values against the prior view snapshot. It emits only changed operations:

```
SetText(node, text)
SetProperty(node, property_id, value)
SetAttribute(node, attribute_id, value)
RemoveAttribute(node, attribute_id)
EnterBranch(region, template_id)
LeaveBranch(region)
ListInsert(region, key, before, template_id, slots)
ListMove(region, key, before)
ListRemove(region, key)
MountChild(region, child_plan)
UnmountChild(region)
```

The host does not receive the model and does not perform a general tree diff on
the fast path.

Application code names conditional structure explicitly with an ordinary
Glamour value, not a lifecycle hook:

```
glamour.branch(
    "account-details",
    model.show_details,
    account_details(model.account),
)

glamour.optional_child(
    "validation-summary",
    validation_summary(""),
    model.problem.map(validation_summary),
)
```

Each stable identity authenticates one structural region and its static
template. Changing the Boolean emits `LeaveBranch` or `EnterBranch`; changing
the `Option` emits `UnmountChild` or `MountChild`. Re-entry constructs only the
authenticated template and then applies changed scalar slots. The implemented
floor separates compiler-declared nodes from live adopted nodes, so a branch or
optional child without nested regions may be absent from the static render and
enter directly at its authenticated position. Its dormant template carries its
authenticated event plans, which the host installs atomically with the subtree
and removes when that subtree leaves.
`optional_child` receives its dormant template separately from the current
`Option`, so absence does not erase the compiler-owned structure or introduce a
placeholder DOM node. A retained live node compares its authenticated event
class/plan bindings separately from scalar attributes and emits
`SetEventPlan`/`RemoveEventPlan` for additions, replacement, and removal.
Publication never weakens event authentication by treating a binding as a
generic attribute update.
Compiler-authenticated region plans retain the ordered identities of following
sibling roots. The host inserts a returning child before the first live
following root, preserving source order across independently removed regions
without adding wrapper elements to application markup.

### Fine-grained work without user-visible signals

The first optimized implementation reevaluates the pure `view` after each
message but:

- static structure is never rebuilt;
- unchanged dynamic slots emit no patch;
- keyed regions retain per-key slot snapshots;
- mapped child views retain their own template instance state;
- equality is specialized by slot type.

This removes DOM and boundary work while keeping semantics simple.

A later compiler optimization may derive model-field dependencies for pure
template slots and skip unaffected slot calculations. That optimization must be
proven semantics-preserving and must fall back to reevaluation when analysis is
uncertain. Authors never write dependency arrays. A missed optimization changes
speed, not behavior.

This split is intentional. Signal graphs are useful runtime machinery, and the
cross-framework TC39 proposal shows convergence on their underlying semantics.
They are not required as Glamour's application-facing state API.

### Scheduling is explicit and bounded

Input, selection, and direct-manipulation messages update synchronously within
the browser event turn unless the application explicitly requests deferred
work. Other message batches may coalesce until the next rendering opportunity.

The runtime:

- prevents reentrant update calls;
- processes messages through one FIFO queue;
- limits work per turn;
- yields between low-priority batches;
- reports long update, view, and patch phases;
- never silently drops messages;
- gives tests a deterministic scheduler.

There is no component-level concurrent rendering contract in 1.0. If
interruptible view evaluation is added later, the model transition remains
atomic and effects from abandoned evaluations cannot run.

## Wasm/browser protocol

### The model stays in Wasm

The application instance owns its model. A browser event passes an event-plan
ID and compact payload into Wasm. Wasm constructs the typed `Msg`, runs
`update`, evaluates the view, computes changed slots, and returns a patch and
effect buffer.

Debug tooling receives model snapshots only through an explicit development
export. Production builds omit that export unless requested.

### A versioned binary protocol replaces render-loop JSON

The protocol uses fixed-width headers and length-delimited UTF-8/byte payloads
in Wasm linear memory. Every buffer includes:

- protocol version;
- build identity;
- application/island identity;
- sequence number;
- operation count;
- byte length;
- optional development trace offset.

The host validates all lengths, integer ranges, UTF-8, operation tags, node
identities, attribute/property IDs, and sequence numbers before applying an
operation. Unknown versions and operations fail closed.

Wasm allocates command buffers from a resettable per-dispatch arena. The host
copies only values whose lifetime exceeds the call. The ABI has explicit
ownership and release operations; neither side retains an unbounded series of
buffers.

The existing JSON ABI remains:

- the readable reference oracle;
- an interpreter/browser test adapter;
- the compatibility host for current applications;
- a differential target for protocol tests.

It is not used by optimized production builds after phase 3.

### Streaming and caching

The loader uses `WebAssembly.instantiateStreaming` when the server supplies the
correct MIME type and falls back to buffered instantiation with a diagnostic.
Artifacts are content-addressed. Immutable Wasm, CSS, and template manifests
receive long-lived cache headers; the HTML shell points to a build identity.

The loader reports download, compilation, instantiation, initialization, and
first-interaction timing independently.

## Effects and capabilities

### Authority remains separate from data

Every effect constructor requires a narrowed capability:

```
fetch.get(fetch_cap, request, GotResponse)
route.push(route_cap, route, Navigated)
clock.after(timer_cap, duration.ms(250), Debounced)
```

Capabilities are available to `start`, `update`, command construction, and
subscription construction. They are not arguments to `initial` or `view`. A
visible policy decision is projected into ordinary public model data before
rendering; an opaque authority token never becomes presentation state.

Capabilities:

- cannot be forged from strings or ordinary records;
- do not serialize into HTML, model snapshots, logs, or resumable state;
- narrow monotonically;
- appear in `witchy caps` output;
- have redacted development labels;
- are checked by the browser host before the corresponding Web API call.

The compiler rejects a static or server-rendered path that tries to serialize a
capability.

### Mount grants bind source authority to browser policy

Every client root and interactive executable is built against one authenticated
`UiRoot` grant from the selected web grant/profile. The grant contains public
policy data only; the build rejects secret fields, host handles, and an absent or
ambiguous grant. Its canonical digest enters the artifact identity and the
private publication graph.

RFC-0109 pins selection to the explicit `web.grants` project path and the closed
one-entry `UiRoot` document shape. There is no implicit grant derived from the
application name, deployment host, development environment, or browser
manifest. Zero-runtime static output is the only web delivery mode that may omit
the grant because it instantiates no application code.

The current `UiRoot.policy` field is an opaque, reviewed application-policy
identity. It binds the selected grant to the build; it is not itself a URL,
method, port, worker, frame, storage, or DOM allowlist. Concrete browser
authority comes only from the compiler-authenticated narrowed-capability table
below. A future structured root ceiling may restrict that table further, but an
opaque policy name is never parsed as authority and never acts as a wildcard.

The compiler specializes `authorize(UiRoot)` against that exact root grant and
records every reachable narrowed capability policy used by an effect,
subscription, port, secret control, compartment, worker, or owned-DOM command.
The specialization must be deterministic and closed. Dynamic capability
selection whose possible policies cannot be bounded at build time fails
production compilation rather than widening the host grant. The generated Wasm
retains the authorization value privately, while effect output contains only a
numeric descriptor from the compiler-owned table.

The artifact manifest publishes a closed non-secret enforcement projection.
Every effect and subscription entry binds its numeric descriptor to exactly one
semantic kind and exact normalized narrowed policy; unrelated descriptors in
the same artifact cannot borrow that policy. Static secret controls and other
non-work authorities receive equivalent compiler-owned entries. The projection
also carries the selected root-grant digest and participates in the executable
artifact identity.

A progressive form is one such static control. Its complete checked action
record — form identity, method, destination, ordered typed fields, and input and
result schema identities — enters `staticControls` in the artifact grant. A
secret field contributes only its `(form, field)` custody coordinate to browser
policy. The external artifact record repeats the action for host discovery, but
the loader requires byte-equivalent agreement with the embedded projection
before it gives the optimized form host that record. Changing an action URL,
field kind, or schema in a public manifest therefore cannot create authority.

At mount, the host authenticates the project grant digest and the complete
artifact projection, derives the placement's instance table from that exact
projection, and refuses a missing descriptor, a kind mismatch, or a request
outside the descriptor's own policy. CSP, Trusted Types, worker, frame, port,
storage, navigation, and resource policies derive from the same table. The
page-level browser header is the least union needed by the instances published
on that route; runtime checks remain per instance and per descriptor. A manifest
edit cannot widen authority because the executable identity, custom section,
manifest projection, compiler-emitted descriptor registry, and placement grant
must agree before Wasm instantiation. Shared executable bytes may serve several
placements, but every placement receives its own instance grant and lifecycle.

Every runnable Wasm carries exactly one `witchy.web.mount-grant` custom section
containing the canonical public grant record and, for an interactive artifact,
its specialized artifact identity plus complete enforcement projection. The
loader compiles the module, reads that section with
`WebAssembly.Module.customSections`, and requires canonical byte-equivalent
closed records before instantiation. A missing, duplicate, malformed, or
mismatched section fails before the runtime receives `UiRoot` policy data. The
external manifest is discovery data, not an authority source: changing it alone
cannot add a descriptor or widen a policy.

### Fetch is structured

Requests distinguish method, safe URL, headers, body, credentials policy,
redirect policy, timeout, and response decoder. A Fetch capability constrains
origins, methods, and path prefixes. The host intersects the command with the
grant and derived CSP.

Response decoders are bounded. Text and byte bodies have configurable maximums;
JSON decoding is typed; streaming is exposed through an explicit stream
capability and subscription rather than buffering without limit.

### Standard browser capabilities have closed production protocols

Storage, workers, compartments, and ports are not aliases for arbitrary
JavaScript. Each uses a dedicated capability kind, a compiler-owned descriptor
and result codec, and one closed production host protocol. A reachable use whose
policy or implementation cannot be resolved exactly fails production compilation.

The 1.0 storage floor exposes `UiStorage`, narrowed from `UiRoot` with four
compiler-visible policy values:

- provider: exactly `session` or `local`;
- namespace: a non-empty ASCII policy label;
- key prefix: bounded UTF-8 with no NUL;
- maximum value bytes: `0..=65536`.

`storage.get`, `storage.set`, and `storage.remove` accept a relative UTF-8 key of
at most 256 bytes. The host rechecks the key prefix and value limit before
touching Web Storage. Physical keys include the authenticated root-grant digest
and namespace, so two applications cannot address each other's entries through
Glamour's storage protocol or collide accidentally. Web Storage remains
origin-shared: unrelated arbitrary JavaScript already executing on that origin
can enumerate it. Confidential data therefore requires secret custody, a
separate origin, or server storage rather than `UiStorage`. The application
receives only the closed `StorageResult` variants
`Missing`, `Value(String)`, `Stored`, `Removed`, and `StorageFailure(problem)`.
Quota, disabled-storage, decoding, and host exceptions become bounded failures;
they do not throw through the scheduler. Storage is an effect, never a read from
`view`, and storage values cannot be secret-custody references.

The 1.0 worker floor accepts only a direct capability-free Witchy task
declaration with compiler-known request and result types. The compiler emits a
content-addressed worker executable and closed request/result codecs, then binds
its artifact identity, schema identities, maximum request/result bytes, maximum
concurrency, and timeout into `UiWorker`. The framework worker host exposes no
DOM, Fetch, storage, ports, nested workers, or application root. A worker gains
additional authority only through a future separately reviewed capability
protocol; ambient worker globals are not authority. Cancellation terminates or
retires the task generation, and a late result cannot enter `update`. The route
policy admits only the same-origin content-addressed framework worker graph and
otherwise emits `worker-src 'none'`.

The 1.0 compartment floor replaces the compatibility
`Compartment(String, String, String)` surface in optimized output with a sealed
typed renderer value. A renderer registry entry binds one content-addressed
same-origin frame artifact, typed grant and event schemas, a byte limit, and its
static fallback. The host creates a sandboxed frame without `allow-same-origin`,
forms, top navigation, popups, downloads, or storage authority. It transfers one
private channel after checking the exact frame window, instance nonce, renderer
identity, and schemas; ordinary global `message` traffic is ignored. Compartment
events are subscriptions owned by the structural frame scope and stop at
teardown. A route with such a renderer admits only its same-origin frame graph;
all other routes emit `frame-src 'none'`.

Production ports come from a project-selected, toolchain-owned adapter registry.
Each entry binds a public name to one audited adapter identity, typed request and
result schemas, byte limits, lifecycle, and a closed browser-authority summary.
The compiler joins a reachable `HostPort(request, result)` descriptor to exactly
one entry. An undeclared port, duplicate name, schema mismatch, or adapter outside
the locked toolchain registry fails the build; production must not defer that
failure until the user invokes the port. The host dispatches by authenticated
numeric descriptor, never by a request string. The existing
`CredentialPort(String)` and string-valued `PortResult` remain compatibility
surface until migrated to this typed registry. Custom JavaScript adapters require
a separate RFC defining package locking, review identity, authority declaration,
and isolation; a module path in project configuration is not sufficient. The
initial locked registry will contain `credential.get-exchange.v1` and
`credential.create-exchange.v1`. Each consumes WebAuthn Level 3 option JSON,
propagates scheduler cancellation through `AbortSignal`, keeps the resulting
credential response in the host, and may post that response only through an
explicitly authorized, build-bound same-origin exchange endpoint. Witchy
receives only a closed bounded outcome containing the HTTP status and success
flag; credential JSON and response bodies never enter `PortResult`, model state,
snapshots, diagnostics, or resumable state. Until the approved relying-party
payload contract lands, production rejects WebAuthn adapters unless
`globalThis.__witchyHostPorts` exposes a callback for the adapter and endpoint;
otherwise it returns a typed failure instead of credential JSON through the
compatibility string port. The adapters
derive the matching
`publickey-credentials-get` or `publickey-credentials-create` Permissions Policy
for each route. The compatibility request codec is bounded to 60 KiB and its
outcome to 128 bytes. The generic `HostPort(request, result)` uses compiler-owned
closed codecs without changing registry or authority semantics.

The initial source surface is closed over that registry rather than accepting an
adapter name from application code:

```witchy
type Auth:
    Auth(HostPort(CredentialExchangeRequest, CredentialExchangeOutcome))

fn authorize(root: UiRoot) -> Auth:
    Auth(glamour.credential_get_exchange(root, "/auth/passkey/exchange"))

fn begin(auth: Auth, options_json: String) -> Cmd(Msg):
    match auth:
        Auth(port) -> glamour.host_port(
            "login.passkey",
            port,
            glamour.credential_exchange_request(options_json),
            fn(result: Result(CredentialExchangeOutcome, String)): PasskeyFinished(result),
        )
```

`credential_create_exchange` has the same types and selects the create adapter.
The endpoint argument must be a compiler-visible same-origin absolute path. The
generic `HostPort` constructor is sealed; adding another adapter requires adding
its typed constructor and audited codec to the toolchain registry. The current
capture transport may use up to 512 bytes for the status-only typed result wire,
while the logical outcome remains one bounded HTTP status and one success bit.

### Host extensions are ports, slots, or compartments

The maintained escape hatches are:

- **port** — a typed request/result protocol implemented by the host;
- **presentation slot** — host rendering with data but no external authority;
- **privileged slot** — an opaque renderer-scoped authority token, implemented
  only after RFC-0100 is revived;
- **compartment** — untrusted or separately trusted content in an isolated
  origin/frame with a narrow message protocol.

Extensions declare lifecycle, authority, serialization, static-rendering, and
test behavior. An extension cannot acquire the application root, arbitrary DOM,
or browser globals through the Glamour API.

## Rendering and delivery

### Static rendering

`witchy build --web` executes the application's static entry on the native
backend and emits:

- HTML files;
- extracted and deduplicated CSS;
- content-addressed assets;
- a route and preload manifest;
- optional interactive-region Wasm, lowered as resumable islands or explicit
  fresh client regions;
- the minimal host loader required by those regions;
- recommended security headers;
- a machine-readable build report.

A page with no interactive regions emits no application Wasm and no Glamour
JavaScript runtime.

The integrated project selects this mode explicitly:

```toml
[web]
delivery = "static"
entry = "src/site.witchy"
```

The entry exports a capability-free `web() -> glamour.Site`. `Site`,
`StaticPage`, `site`, and `static_page` are ordinary Glamour types and
functions. The compiler evaluates the entry through its authenticated checked
module, accepts only the closed `glamour.Site` constructor identity, and emits
canonical route files. A static build does not require client index scaffolding
or a source-authored browser protocol manifest. The post-build audit rejects
JavaScript or Wasm artifacts in this mode.

Content-oriented sites may declare a read-only build input without granting the
entry a file capability:

```toml
[web]
delivery = "static"
entry = "src/site.witchy"
content = "../../content"
```

That form changes the authenticated entry to
`web(content: glamour.StaticContent) -> glamour.Site`. The build tool snapshots
the declared directory as sorted, normalized relative paths plus UTF-8 text,
rejects symlinks, special files, oversized files/collections, and records every
input's size and digest in the manifest and build report. `StaticContent`
contains ordinary closed values, not handles: application code can enumerate or
look up the snapshot but cannot read another path, observe time/environment, or
perform I/O. The no-argument entry remains the preferred contract when all
content already lives in Witchy source.

`Site` also owns its static resource graph through ordinary Witchy values:

```
pub fn web() -> glamour.Site:
    let styles = css".card { display: grid; }"
    let logo = glamour.asset_url_or_empty("/logo.svg")
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

Style route ownership and critical routes are explicit build data. The
publication boundary recomputes each checked sheet identity, validates scoping
and class declarations, inlines critical rules, and content-addresses extracted
non-critical rules. Preloads are local, route-scoped, kinded, and must resolve
to an emitted file. A `StaticAsset` resolves beneath `web/public`, rejects
symlinks, suppresses the unhashed source-name copy, emits a content-addressed
file, and rewrites matching checked HTML URLs and preloads. CSS `url(...)`
values are available only through a sealed `css_asset` interpolation in an
image-valued declaration. The publication boundary requires the local logical
URL in `StaticAsset` and rewrites it to the same content-addressed output.

Static rendering is deterministic for the same source, lockfile, declared
inputs, and compiler version. Time, environment, files, network, and randomness
remain capabilities and must be declared as build inputs.

The Witchy book exercises the content form against all 43 files in `book/src`.
Its native entry renders every route through one Witchy view. The counter and
every browser-runnable Witchy fence embed an ordinary `Program` through
`Interactive`; production emits complete server HTML for each counter or editor
cell and lowers it to a resumable island. Each interactive route selects a
content-addressed exact-subset manifest, while byte-identical programs share one
authenticated executable and artifact record across placements. The runnable
host waits for load activation, adopts the existing editor controls without
replacing compiler-owned DOM, and sends edited source only to the opaque-frame
execution boundary. The maintained bundle test builds all 56 canonical routes,
authenticates every declared content input, checks each route-manifest/DOM join,
and requires every editable fence to have one load-activated resumable placement.
It derives the non-runnable route set from `book/examples.json` and requires
those outputs to contain no script, Wasm loader, runnable marker, or island. The
final bundle report records every host artifact and digest. The book omits a
parent CSP because CSP inheritance would block the nonce-authenticated bootstrap
in its opaque `srcdoc` frames; each frame receives the stricter capability-derived
CSP before untrusted code runs. Referrer, content-type, permissions, and Wasm
MIME headers remain. The packager records a canonical deployment base, rewrites
checked HTML URL attributes under it, and the generated island loader resolves
logical route-manifest and artifact URLs against its own content-addressed module
location. GitHub Actions derives the repository project path automatically;
local serving remains rooted at `/`.

### Client applications

`client(root_program)` builds a single application Wasm and host loader. The
initial model is created in Wasm by `initial(Start)`. After authorization the
runtime derives and validates the live view and evaluates `start(auth, model)`
plus initial subscriptions. It commits the root template and private state
before the host performs the startup command or starts a subscription.

Client mode is appropriate for sustained interaction. It is not the default
for content that can be HTML.

### Interactive regions and resumable island delivery

An interactive region is an explicit delivery boundary represented by the
ordinary sealed `Interactive` value described in the user model. It embeds a
complete `Program` in otherwise static content. The application author does not
construct an island, write a second view, select a program through a string, or
manage a separate island state API:

```
type Model derive(PublicState, Reflect, Deserialize):
    query: String
    results: List(SearchResult)

pub fn search_box(initial: Model) -> glamour.Interactive:
    glamour.interactive(Search.program(), initial)

pub fn web() -> glamour.Site:
    let initial = Model("", [])
    glamour.site([
        glamour.static_page(
            "/",
            html"""<main>${glamour.embed(search_box(initial))}</main>""",
        ),
    ])
```

The ordinary source signature is:

```
pub fn interactive(
    app: Program(auth, model, msg),
    initial: model,
) -> Interactive where
    model: PublicState,
    model: Reflect,
    model: Deserialize
```

The `Program` and initial model share the same `model` parameter, so mismatched
state fails in ordinary Witchy type checking. The program's capability-free
`view(initial)` renders the static HTML and becomes the authenticated baseline
for browser updates. `authorize`, `start`, effects, and subscriptions do not run
during this build projection; `initial` is not called because the explicit
public model is the selected state. A program whose view references browser-only
code fails at that source reference with guidance to move the fact into public
model data, use an inert fallback and controlled fresh start, or select a client
root.

`derive(PublicState)` supplies the sealed boundary proof; `Reflect` supplies the
canonical initial-state encoding and `Deserialize` supplies checked state
reconstruction until the `PublicState` derive owns that codec directly. A custom
public model therefore derives `PublicState`, `Reflect`, and `Deserialize`.
`glamour.interactive` returns one sealed, closed `Interactive` type so a site
can contain heterogeneous regions. `glamour.embed` places it in static `Ui` and
erases its message type only after the compiler authenticates the constructor,
program factory, model, view, codec, builder chain, and placement. The compiler
consumes that value into a sealed `IslandPlan` during checked publication,
specializes the resume adapter, and records the declaration identities and
three concrete runtime type identities in the private build graph. Those type
identities enter the executable artifact hash, so a state-ABI change cannot
retain an old artifact identity. An indirect or dynamically selected program
expression, a forged `Interactive`, or an unplaced value is rejected at the
publication boundary.

`glamour.client_region` authenticates the same `Program` plus one event-free
`StaticUi` fallback. Its lowered plan contains no model snapshot or public-state
codec and uses the compiler-owned fresh-mount entry. The manifest, build report,
and developer tools distinguish that plan from a resumable one.

`Interactive` defaults to `OnVisible`. `.activate(...)`, `.prefetch(...)`, and
`.named(...)` are advanced delivery controls. A diagnostic name is optional and
never establishes runtime identity or authority. Activation uses the closed
`Activation` values; prefetch uses the distinct closed `Prefetch` values.
Prefetch may retrieve and cache immutable code; only activation may instantiate
it, call `authorize`, start subscriptions, or dispatch a message.

The lowered resumable plan contains only an optional inert diagnostic name,
activation and prefetch data, canonical public state, Glamour-rendered static
output, and a sealed inert render graph. That graph retains element/text/keyed
structure plus typed event identity, class, decoder kind, and control flags; it
retains no message value or decoder closure. The fresh plan substitutes its
checked, event-free `StaticUi` and contains no model state or live event plan.
Its executable contains the compiler-authenticated live template and event
schemas used to validate the first render and install the post-mount registry.
Legacy `on` and `on_input` bindings fail at a resumable boundary in favor of
typed `on_event`. A plan contains no capabilities, function values, or runtime
closure bag. Native publication derives nonzero numeric node, event-class, and
event-plan identities from the authenticated executable identity, semantic key
scope, child path, and event facts; collisions fail publication. The compiler
derives the executable island identity from authenticated declaration
identities, concrete types, template schema, and protocol version. The optional
name is used only for diagnostics and duplicate detection; it is never an
export name or authority selector.

Each executable also receives a distinct compiler-owned event-registry
identity derived from that authenticated artifact identity. Publication binds
every event plan to the registry, the specialized adapter accepts only that
registry identity, and the host rejects a crossed registry before Wasm
instantiation. Per-route placement identities remain separate so shared
content-addressed code never collapses live island ownership.

The native static checker then derives the external activation manifest,
artifact-side resume registries, per-route instance identity, and canonical HTML
markers from those records. Stable source event IDs are temporary authenticated
join keys: publication consumes each exactly once and leaves only numeric node
identities in HTML. No page or manifest is written until the matching executable
artifact exists.

The first executable floor specializes a per-island adapter against the exact
checked application module and stores content-named Wasm beside the private
publication graph. It decodes an authenticated resume activation frame,
reconstructs a scalar or derived-record public model inside Wasm, validates the
complete event-plan/template-instance/event-class tuple, invokes the typed Witchy
decoder and update, revalidates the live render against the sealed graph, and
emits changed binary text, property, attribute, boolean, typed URL, class, and
ARIA sinks. Compiler-owned keyed-region and key identities also let the adapter
emit LIS-minimal `ListMove` and authenticated `ListRemove` records. For each
initial keyed child, static evaluation also emits
a compiler-private value-bearing template graph. The native checker bounds it,
validates it exactly against the value-free resume shape, assigns a stable
template identity, and publishes only inert host node records. The adapter
retains the authenticated baseline render, so a removed initial key can re-enter
through `ListInsert` and receive scalar patches in the same frame without VNode
JSON crossing the browser boundary. Static checking assigns stable nonzero slot
IDs to text, property, attribute, boolean, typed-URL, class, ARIA, typed custom
property, and compatibility values outside nested dynamic subregions. The
compiler publishes that closed table and emits exact structural slot payloads
for keyed, branch, and optional-child re-entry. Protocol minor 2 additionally
emits numeric `SetCustomProperty` updates against the same closed registry. The
host validates bounded payloads and fills only detached template nodes before
commit. Nested dynamic subregions retain their own authenticated patch plans.
The compiler retains a non-`Nil` authorization value in private island state
and reuses it for every update and subscription reconciliation; it never enters
the public model frame or host manifest. Reachable direct command and
subscription constructors lower to authenticated numeric descriptors, bounded
private callback environments, typed completion codecs, and binary host-work
records. Startup work is staged until fresh Mount commit or the first resumed
activation dispatch. `Cmd.map` and `Sub.map` now compose each statically known
mapper chain into a final compiler-owned descriptor and callback ordinal. The
generated adapter persists only a bounded nested data environment, recursively
decodes the child result, applies each typed mapper inside Wasm, and emits the
root `Msg`; dynamically selected mappers and capability-bearing mapper captures
fail during source authentication. Generated-Wasm coverage exercises nested
command and subscription maps, captured ordinary values, and an asynchronous
HTTP result through the final mapped callback. Structural support covers
authenticated initial keys, branches, optional children, nested initial-key
re-entry, and new keys covered by one homogeneous event-free flat template.
Optimized descriptors also publish collision-checked owner declarations, while
the artifact publishes the closed root/key/branch/child owner-instance table.
Authenticated event plans carry their structural owner; commands inherit that
declaration and live instance, stable command identities are local to it, and render
reconciliation removes private callbacks and cancels work before a departed
owner can complete. Generated-Wasm coverage runs the same stable timer identity
under two keyed owners, removes one owner, preserves the sibling, and rejects
the removed owner's late completion. The same fixture proves that
`glamour.detach` resets one child command to root ownership, preserves it across
the keyed-owner removal, and accepts its later typed completion. Application
subscriptions reconcile independently instead of inheriting whichever event
most recently ran. Each authenticated subscription descriptor plus stable identity
receives its own private live owner instance, so repeated uses of one constructor
can remove one identity without restarting or tearing down its sibling.
Route and resource constructors now carry the stable owner declaration assigned
to their source declaration. The compiler inserts that identity, fused
`Cmd.map` lowering preserves it, and the generated adapter revalidates it
against the authenticated descriptor before staging host work while retaining
the causal live owner instance separately. Generated-Wasm coverage exercises
HTTP and navigation constructors from distinct declarations and verifies their
distinct nonzero owner scopes through publication and dispatch. Private
late-emission coverage rejects a consumed one-shot completion, removes a
departed structural owner's callback before its completion, keeps replaced and
removed subscription generations inert in the host, and rejects dispatch after
Wasm state disposal.
Authenticated event bindings and nested regions are restored with an inserted
authenticated subtree. Retained nodes emit authenticated event-plan addition,
replacement, and removal records independently of scalar sinks.
Production publication writes the Wasm, closed manifests, content-addressed
runtime module graph, rewritten pages, and required browser headers through one
staging directory and swaps it into place only after an island-aware audit.
Byte-identical compiler-generated island modules share one content-addressed
executable while retaining separate instance records. Built-in timer, interval,
same-origin request, and navigation handlers revalidate the narrowed request;
an undeclared production port has no implementation and completes with a typed
error rather than acquiring ambient host authority.

The first fresh executable floor separately evaluates the direct authenticated
`initial` and `view` declarations with the static route's exact public `Start`
value. The resulting private model exists only during capability-free compiler
evaluation and inside Wasm; publication retains the inert `StaticUi`, checked
live template/event schemas, and public `Start`, never model state. On
activation the adapter strictly decodes that `Start`, regenerates the model,
validates the live graph, and emits one binary root `Mount`. The host plans the
complete frame before atomically replacing the fallback and installing template
events, then accepts ordinary typed patches. The live root template retains its
initial keyed, branch, and optional-child regions; dormant event-bearing
templates enter through the ordinary authenticated structural patch protocol.
Compiler-generated effect and subscription records remain required.

The next keyed floor permits new application keys only in a list region whose
initial children prove one homogeneous scalar template. The compiler publishes
one event-free, nested-region-free dynamic prototype for that region. Protocol
minor 3 carries bounded UTF-8 source keys as inert region-map data and adds
dynamic insert, move, remove, and exact slot-update records. Cloned node IDs stay
local to each entry rather than entering the global node namespace; retained
entries update through their template's closed slot table and are never
remounted merely because scalar values changed. Regions without a proven
homogeneous prototype retain the closed initial-key behavior.

`glamour.interactive(app, initial)` and its typed builder methods are the Phase
6 source contract. `IslandPlan`, manifests, and adapters are authenticated
lowerings, not a second source-level component model. Any future syntax sugar
must expand to `Interactive` and requires a separate general-purpose language
RFC.

Each lowered island instance has its own:

- Wasm artifact or shareable module group;
- model;
- template instance table;
- message queue;
- capability grant;
- lifecycle;
- build identity.

The static renderer emits the interactive region's HTML plus inert metadata
containing template IDs, node IDs, event-plan IDs, artifact locations, and—for
a resumable region—public model state. A tiny delegated loader observes
activation triggers. It does not execute application logic before activation.

The loader validates a closed external manifest against the existing DOM before
registering policy. A prefetch boundary may fetch and cache the immutable
artifact but cannot instantiate it or acquire authority. Activation is the
first operation allowed to instantiate application code, create the program's
authorization record, or start application work. A matching delegated browser
event is reduced to its authenticated plan/node IDs and allowlisted value,
checked, key, composition, and user-activation fields. Browser `Event`, target,
and DOM handles never enter the island. For a resumable region, the first
snapshot is passed to resume exactly once; later snapshots received while
loading enter a bounded per-island queue.

An authenticated interaction is also a latency backstop for a pending
resumable region. If `OnLoad`, `OnIdle`, `OnVisible`, or `OnMedia` has not yet
completed activation, the first matching event promotes activation immediately
and enters the same bounded queue exactly once. This closes the observer and
idle-callback race without changing the declared prefetch policy. It is safe for
the resumable form because the static event graph already authenticates the
typed message. The event-free `client_region` rule below remains distinct.

A dormant event plan may suppress meaningful native navigation or submission
only when publication also authenticates a progressive fallback for that exact
default action. The loader queues the typed event once; if artifact retrieval,
instantiation, grant binding, or activation fails before commit, it performs the
checked fallback once instead of leaving a prevented link or form inert. Without
such a fallback, the compiler rejects deferred interception of that native
default. Events with no meaningful native default need no fallback. An event
plan's explicit propagation decision remains visible in diagnostics because it
cannot be undone after dispatch.

The public event record admits only two closed fallback forms. `navigate`
binds the checked event node to its statically checked `href`. `submit` binds a
form or its descendant submit control to the form's checked action and canonical
`get` or `post` method; a submit control that names a non-ancestor form is
rejected until the compiler can authenticate that relationship. The fallback
record participates in the build identity. Before recovery, the loader
revalidates the live element and attributes against that record. It then invokes
the corresponding native click or form submission under a one-event bypass so
the delegated activation listener cannot intercept its own recovery. There is
no application-selected JavaScript fallback callback. Each prevented event owns
one consumable recovery record: activation success discards it, while a
pre-commit activation failure consumes it exactly once.

A `client_region` fallback has no event plan because `StaticUi` is event-free.
Its first `OnInteraction` event is therefore an activation gesture, not an
application message, and is not replayed after mount. The loader does not cancel
that gesture's native browser behavior. Once the fresh view is mounted, its
authenticated events use the ordinary bounded queue. An application that must
handle the first gesture as a typed message uses resumable `interactive`, or
activates the fresh region before interaction with `OnLoad`, `OnIdle`,
`OnVisible`, or `OnMedia`.

On resumable activation:

1. the loader verifies the build and island identity;
2. it instantiates or reuses the matching Wasm module;
3. the island decodes the interactive region's `PublicState`;
4. it binds to existing DOM identities;
5. it calls `authorize` with the region's narrowed `UiRoot`;
6. it evaluates `start(auth, model)` and the declared subscriptions into inert
   checked descriptors;
7. it commits the adopted private state and, when activation was promoted by an
   interaction, appends that authenticated event as the oldest application
   message;
8. it handles the triggering event exactly once before any startup completion or
   initial subscription emission can become observable, then the host starts the
   staged work;
9. it patches only subsequent changes.

This ordering is a protocol barrier, not a timing assumption. Starting host work
may be asynchronous or may synchronously produce a value; either way, its first
message is queued behind the activation event. Activation without a triggering
application event starts the staged work immediately after commit. A fresh
`client_region` interaction remains activation-only, so it has no application
message to place before startup work. Client roots use the same commit-before-
work rule without an activation event. No host callback may reenter update.

Before publishing host work, the adapter reconciles the staged startup set with
the command and post-update subscriptions produced while handling the triggering
event. The post-event subscription set is authoritative. A cancellation,
replacement, or removal produced by that event suppresses the superseded staged
item without briefly starting it; remaining startup commands precede
event-produced commands in deterministic FIFO order. The adapter commits every
surviving private callback environment and live owner instance before emitting
the combined work frame. The host applies DOM work, then cancellations and
removals, then starts the remaining effects and subscriptions. A synchronous
host result still enters only through the non-reentrant FIFO completion path.

It does not rerun the initial template merely to discover listeners. A mismatch
between the authenticated DOM graph and the artifact may perform a controlled
fresh render only after the build identity, public-state frame, codec, and
artifact identity have all validated. That render uses the already decoded
explicit public model; it never substitutes `initial(Start)`, because the
embedded model may intentionally differ from the program's client-root initial
state. It then calls `authorize`, evaluates `start(auth, model)` and initial
subscriptions, commits the controlled rebuild, and only then exposes that work
to the host. It is reported as `fresh-from-public-state`, not resumed. A build,
codec, artifact, or state-frame mismatch fails closed or reloads the current
document; it never guesses across versions. Successful resumption likewise does
not call `initial`: the explicit public initial model is already the program
state. It does evaluate `start(auth, model)`, so authorized startup commands
have the same post-commit semantics as a client root.

A declared `client_region` follows its distinct fresh-start path intentionally.
After DOM, artifact, and fresh-input validation it creates authorization, calls
`initial(Start)`, derives the live view, and evaluates `start(auth, model)` plus
initial subscriptions. The adapter validates the compiler-authenticated live
template and event schemas, then commits the replacement of `StaticUi`, private
state, and post-mount event registry atomically. Only after that commit does the
host perform the startup command or start subscriptions. Its activation gesture
is not reinterpreted as an application event because the fallback declared no
authenticated event mapping.

The stateful browser ABI provides distinct compiler-owned resume and fresh
entries. Resume installs private state derived from the checked public-state
frame, emits zero initial bytes, and leaves the output sequence at the first
patch. The host then sends either the authenticated triggering event or an
empty activation-commit frame as the first input-sequenced dispatch; that one
dispatch reconciles and publishes staged startup work. Fresh accepts a
separately authenticated `Start` frame with no model
snapshot and emits the controlled initial mount plus its exact post-mount
registry. The optimized host adopts every compiler-declared node and region
through bounded child paths, rejects aliases, omissions, and incompatible event
bindings before application instantiation, accepts the activation loader's
first scalar event only when its node, class, and plan match that adopted
registry, and applies later keyed moves, removals, and authenticated initial-key
re-entry. Re-entry stages the compiler-emitted inert subtree before validating
subsequent scalar operations, so the complete frame commits atomically. Checked
initial subscription records start only after private Wasm state is live and
share ordinary disposal.
The compiler emits the closed callback/descriptor tables, private capture
environments, mount-grant specialization, and effect/subscription adapters. The
native writer publishes and audits the complete per-island plans atomically.
Phase 6 still requires the acceptance evidence and remaining policy/lifecycle
rows below; publication itself is no longer intentionally disabled.

A resumable interactive region's initial model must derive `PublicState`. The
derivation rejects capabilities, secret references, host handles, functions,
streams, and types without a stable wire representation. Sensitive state stays
server-side or starts client-only. A non-resumable client region may render an
inert fallback and perform a controlled fresh start, but it is reported
distinctly and cannot claim resume behavior.

The first implementation loads at island granularity. Per-function or
per-message Wasm splitting requires independent evidence that its request and
code-size overhead improves real applications.

### Server mode

Server mode is an optional Witchy host that renders routes and performs typed
actions. It uses the same `Program`, templates, route definitions, request
decoders, and capability discipline.

Server-only values cannot cross to the browser unless transformed into an
explicit `PublicState` or response value. A type-level side marker prevents a
server secret or capability from entering a client template.

The protocol is documented and self-hostable. Static and client modes remain
fully supported.

### Progressive enhancement

Forms and links produce useful HTML behavior before an interactive region
activates:

- links have real `href` values;
- forms have real methods/actions where server behavior exists;
- buttons use native semantics;
- validation has server and client implementations;
- focus and scroll restoration follow platform behavior.

Glamour enhances navigation and forms after activation, but does not require
JavaScript/Wasm for basic content access when the application declares a
progressive route.

The shared `decode_form_entries` function checks raw key/value entries against
the same `FormSchema` that rendered and published the form. It rejects
duplicates, unknown fields, missing required fields, and invalid field kinds,
and partitions server secret values from ordinary model-safe values.

The optional capability-free `glamour_server.form_action` adapter turns that
schema into a `std/server` handler. Before invoking the application's typed
callback it checks the declared method and local action path, configured body
limit, URL-encoded content type, strict percent escapes, and a configured
same-origin `Origin` or `Referer` policy for POST. Application callbacks may
capture explicit server authority; the adapter itself owns none. Browser
decoding uses the same checked fixture corpus and deterministic problem order.
It accepts bounded string entries only, keeps secrets in a private
non-serializable host map, and erases a secret on first read. The optimized
browser host installs one delegated form boundary when its checked action
manifest is non-empty. Same-origin forms move through explicit validating,
submitting, succeeded, failed, and cancelled records; a newer generation
aborts and invalidates stale work. Cross-origin and submitter-overridden forms
retain native behavior. Secret fields require POST, remain absent from
lifecycle values, and pass directly from `FormData` into the host-owned
same-origin request. Protocol 1.4 derives nonzero input/result schema identities
at the compiler boundary; source manifests cannot supply them. The optimized
host encodes ordered public fields into `ActionInput` frames, omits secret
fields, and returns a closed status plus optional HTTP status in
`ActionCompletion` frames. Both enter the ordinary Witchy dispatch boundary,
are generation- and sequence-bound, and contain no JavaScript callback or
response body. Glamour independently derives the same identities from the
typed `FormSchema`, strictly decodes the complete binary frame, and returns
closed `ClientActionInput` or `ClientActionCompletion` values. Public values
retain their declared text, email, number, or checkbox variants; secret fields
have no public variant. The compiled optimized-counter fixture proves input and
completion frames drive ordinary Witchy model transitions without a
source-authored action/schema ID or application JavaScript callback.

The static `Site` owns the progressive action graph. `site_with_forms` accepts
checked `FormSchema` values, and `form_attributes` emits the same stable schema
identity into the rendered form. The compiler boundary revalidates method,
safe action URL, unique field names, field kinds, required facts, and rendered
binding agreement before emitting the public action manifest. A secret field
appears only as a `secret` schema fact; its value never enters `Site`, HTML, or
the manifest.

## Router, data, and forms

### One typed route graph

Routes are declared once and compile to:

- a parser from browser/server paths to a route variant;
- a formatter from a route value to `SafeUrl(Navigation)`;
- static output paths;
- server handlers;
- client navigation tables;
- preload boundaries;
- route-specific capability requirements.

Unknown routes and parse failures are explicit variants. String concatenation
is not the maintained way to build internal links.

Nested layouts are pure view composition. Loaders return `Resource` values and
commands. Redirects are typed route values.

### Data loading has ownership and cancellation

A loader belongs to a route or feature scope. Navigation cancels obsolete work.
Cache keys, freshness, retry, and invalidation are explicit policies.

The default cache is:

- bounded;
- partitioned by credential policy and origin;
- observable in development tools;
- incapable of storing host-custodied secret material;
- disabled for responses that are not explicitly cacheable.

### Forms are typed state machines

A form schema defines:

- field names and types;
- parsing;
- synchronous validation;
- optional async validation;
- initial values;
- error messages;
- submission payload;
- server action;
- accessibility relationships;
- secret-custody fields.

The compiler checks that labels and errors connect to controls, that a submit
path exists, and that secret fields do not enter the model. Validation runs from
the same schema on client and server.

Forms expose explicit `Editing`, `Validating`, `Submitting`, `Succeeded`, and
`Failed` states through ordinary model values. Double submission, stale
validation, and cancellation have framework defaults and can be overridden.

## Styling

### Checked CSS literals

Glamour adds a compile-time `css` literal:

```
let styles = css"""
  .card {
    display: grid;
    gap: ${space.md};
    color: ${theme.foreground};
  }
"""
```

The compiler:

- parses CSS;
- reports source-mapped syntax errors;
- scopes local selectors with stable semantic hashes;
- returns typed class handles;
- validates property/value categories where the grammar is stable;
- rewrites asset URLs through the build graph;
- extracts static CSS;
- deduplicates shared rules;
- records critical route CSS;
- rejects unsafe dynamic constructs and undeclared remote URLs.

Dynamic style values use typed custom properties. The host sets a known
property ID to a validated value; applications do not construct style strings.

Global CSS is explicit. The build report shows every global rule and its owner.

The implemented floor emits scoped text and typed class handles. It accepts a
sealed `CssImage` only as the complete value of `background-image`,
`border-image-source`, or `list-style-image`; direct `url(...)`, arbitrary
interpolation, undeclared assets, remote assets, and non-canonical paths fail.
Static publication rewrites the checked logical path to the content-addressed
asset. Color, length, number, percentage, angle, and time declarations select
distinct sealed `CssValue` categories at expansion time. Typed
`CssProperty(kind)` values
produce category-preserving `var(...)` references and assignments; static HTML
admits only bounded `--glamour-*` declarations with a closed typed-token/var
token grammar. Optimized islands retain these assignments as distinct typed
values, assign compiler-owned numeric sink IDs, emit protocol-1.2
`SetCustomProperty` records, and resolve them through a closed host registry;
generic dynamic `style` strings remain unavailable. Deliberately unscoped rules
use the separate `global_css` tagged literal. Each global selector retains its
tag invocation origin and route attachments in the production manifest and build
report; ordinary `css` remains scoped by construction. Broader
property-category validation remains later work under this contract.

### Design systems are Witchy modules

A design system exports:

- typed tokens;
- class handles;
- pure view functions;
- feature modules when behavior is required;
- accessibility contracts;
- visual test fixtures.

It does not require a second templating language or a JavaScript build plugin.

## Accessibility

Accessibility is a compile-time, component, test, and documentation concern.

### Static checks

The template checker diagnoses:

- images without an accessible text decision;
- controls without names;
- labels that do not resolve to controls;
- invalid or contradictory ARIA roles/attributes;
- click handlers on non-interactive elements without keyboard semantics;
- missing button types in forms;
- invalid heading and landmark structures where statically knowable;
- positive tab indices;
- autofocus and motion patterns that require explicit acknowledgement;
- keyed list mistakes that can destroy focus.

Some checks are errors; context-dependent checks are warnings with a precise
suppression carrying a reason.

### Accessible primitives

Glamour ships maintained primitives for dialogs, menus, tabs, listboxes,
tooltips, disclosure, comboboxes, toasts, and focus management. Their behavior
follows WAI-ARIA Authoring Practices where ARIA is necessary and prefers native
HTML where it is sufficient.

Each primitive includes:

- keyboard interaction tests;
- focus entry/return behavior;
- reduced-motion behavior;
- screen-reader name/description assertions;
- high-contrast and zoom fixtures;
- static and resumed-island tests.

### Automated and human gates

Browser conformance runs automated accessibility checks, keyboard-only flows,
200% and 400% zoom snapshots, forced-colors mode, and reduced-motion mode.

Before 1.0, the book, starter, and one representative application receive
manual testing with keyboard navigation and current screen readers on supported
desktop/mobile platforms. Automated tools do not replace this gate.

## Security model

### Safe sinks

The production DOM host has a finite operation vocabulary. It may:

- create allowlisted HTML/SVG nodes;
- set checked text, boolean, token, property, URL, ARIA, and style values;
- insert, move, and remove owned nodes;
- install delegated framework events;
- invoke explicitly registered slots/ports/compartments;
- manage focus/measurement through narrow UI commands.

It may not:

- evaluate strings as code;
- assign application strings to HTML-parsing sinks;
- install string event handlers;
- navigate to unchecked URLs;
- reach nodes outside its root;
- retain stale Wasm memory views;
- dispatch a privileged extension by name alone.

Trusted Types and a derived CSP enforce the same boundary at the platform layer.

### Capability-derived browser policy

The build manifest derives:

- `connect-src` from granted Fetch origins;
- script and worker sources from content-addressed application artifacts;
- frame sources from compartment renderers;
- image, media, and font sources from typed asset/URL policies;
- style policy from extracted CSS and declared dynamic style support;
- Trusted Types policy names from actual framework sinks.

Production static publication records this derivation independently for every
route in both the public manifest and build report, then emits the same policy
as route-scoped response-header data. Routes without executable instances use
`script-src 'none'` and `connect-src 'none'`; interactive routes admit only
same-origin script and connection sources plus the narrow
`'wasm-unsafe-eval'` source required to compile authenticated Wasm artifacts at
the CSP layer. The authenticated
publication graph, content-derived filenames, loader manifest checks, Wasm
custom section, and per-artifact grant independently restrict that broad
same-origin CSP class to the exact content-addressed runtime graph. The build
report distinguishes browser source-class enforcement from artifact-identity
enforcement rather than claiming that `script-src 'self'` authenticates bytes.
Absent worker and frame grants produce `worker-src 'none'` and
`frame-src 'none'`. Critical inline styles use exact SHA-256 sources. A checked
dynamic custom-property attribute receives an exact `style-src-attr`
`'unsafe-hashes'` source instead of admitting arbitrary inline style. The
optimized DOM host has no HTML-string sink, so its
production policy requires Trusted Types for script sinks while declaring no
application-created policy (`trusted-types 'none'`).

Development mode may need a wider local policy for live reload. The development
server prints that difference and production builds never inherit it.

Hosting profiles distinguish **required**, **available**, and **degraded**
platform enforcement. `[web].hosting = "portable"` is the default and explicitly
accepts degraded response-header enforcement; `[web].hosting =
"headers-required"` makes the complete emitted response-header set a deployment
requirement. The selected profile enters the manifest and build report.
`witchy doctor --web --deployment <url>` must reject a `headers-required` deployment whose
observed headers do not exactly provide the route policy. For a static host such
as GitHub Pages, portable publication
inserts the route's exact meta-compatible CSP into the document head and audits
that it agrees with the manifest and report. Directives that browsers ignore in
meta delivery, including `frame-ancestors` and Permissions Policy, remain in
`_headers` for capable hosts and are named as unavailable by the portable
profile. The publisher does not claim equivalent enforcement. An application
whose threat model requires those response headers selects `headers-required`
and cannot promote a deployment until the deployed-header check passes.

### Secret custody

Passwords, private keys, credential responses, and equivalent values remain in
host-owned controls and ports. Application models receive only status,
non-sensitive metadata, or opaque non-serializable references.

Static rendering, debug snapshots, time travel, error overlays, and resumable
state all reject secret references.

### Supply-chain and reproducibility

The default project has no npm install step. The Witchy toolchain embeds the
audited host loader and browser schema. Project dependencies remain pinned by
the Witchy lockfile and verified by content identity.

The build emits:

- compiler and runtime versions;
- dependency identities;
- capability and CSP summaries;
- public artifact hashes;
- source-map policy;
- reproducibility inputs;
- a software bill of materials.

### Protocol and host hardening

The binary decoder, event decoder, URL handling, static-state decoder, patch
application, and island resume path receive:

- property tests;
- structured fuzzing;
- malformed/truncated buffer tests;
- integer overflow and memory growth tests;
- stale sequence and use-after-unmount tests;
- cross-island identity tests;
- differential tests against the JSON reference host;
- browser security probes under the production CSP.

A malformed guest buffer terminates that application root without granting
additional authority or mutating outside the owned root.

## Developer experience

### One maintained command path

The standard workflow is:

```
witchy new --web my-app
cd my-app
witchy dev
witchy test
witchy build --web
```

`witchy dev`:

- chooses an available loopback port;
- serves the exact local project;
- watches source, templates, CSS, and declared assets;
- incrementally compiles;
- updates CSS without reloading;
- hot-swaps pure view/update code when model and message schemas are compatible;
- migrates state only through a compiler-checked generated migration;
- performs a full reload with an explanation when preservation is unsafe;
- shows diagnostics in the terminal and browser;
- never opens network authority beyond loopback unless requested.

There is no required configuration file for the starter.

### Diagnostics describe the model

Errors point to Witchy source and explain:

- the expected template slot type;
- the actual value type;
- the capability needed to construct an effect;
- why a value cannot enter public resumable state;
- which route or feature owns a failed command;
- which static accessibility rule was violated;
- whether an error arose in update, view, decoding, host validation, or patching.

Generated JavaScript/Wasm names are secondary details behind expandable
diagnostics.

### Source maps and stack traces

Development builds map:

- Wasm instruction offsets to Witchy functions and expressions;
- template IDs and slot IDs to literal holes;
- event-plan IDs to handler expressions;
- CSS hashes to source selectors;
- route IDs to declarations.

The browser overlay shows the message that was being processed, a redacted
model diff, commands emitted, patches attempted, and capability checks. It does
not expose secrets.

### Glamour developer tools

The browser tools expose:

- the application/interactive-region tree and lowered island instances;
- current model shape and redacted values;
- the message timeline;
- model diffs;
- command/subscription lifecycle;
- capability grants and denials;
- template and dynamic-slot updates;
- DOM operation count and duration;
- resource cache state;
- route transitions;
- static/resume/fresh-mount decisions;
- time travel in development when all traversed values are replayable.

Time travel replays messages against recorded command results. It never repeats
an external command by default.

### Documentation and examples

The maintained learning path contains:

1. static page;
2. counter with model/message/update/view;
3. form and validation;
4. fetch with typed resources and cancellation;
5. nested routes;
6. reusable feature composition;
7. styling and design tokens;
8. static rendering and an embedded interactive program;
9. capabilities, ports, secret custody, and compartments;
10. testing, profiling, accessibility, and deployment.

Each example is compiled, run on both backends where applicable, exercised in a
browser, and kept in the public repository.

The Witchy book remains the first production dogfood application. It must use
the same public APIs as external applications.

## Testing model

### Pure tests

The framework test package provides:

- `update` table tests;
- message-sequence tests;
- command assertions;
- subscription identity assertions;
- view queries over typed template output;
- route parse/format round trips;
- form schema and validation tests;
- static rendering snapshots.

Tests do not need a browser for pure application logic.

### Host simulation

A deterministic host simulator supplies:

- virtual time;
- scripted Fetch responses;
- route history;
- port results;
- stream events;
- cancellation;
- capability denials.

It runs the same event/command protocol as the browser host and supports
property-based message sequences.

### Browser conformance

The repository maintains a browser matrix for current stable Chromium,
Firefox, and WebKit, plus defined mobile coverage. Tests cover:

- mount, update, unmount, and remount;
- text, property, URL, CSS, branch, child, and list patches;
- event decoding and delegation;
- IME composition, selection, focus, scroll, and autofill;
- history and navigation;
- static output and progressive forms;
- island activation and resume mismatch;
- async cancellation and stale results;
- accessibility primitives;
- CSP, Trusted Types, compartments, ports, and secret custody;
- streaming and fallback Wasm loading;
- memory growth and repeated navigation.

### Twin-backend parity

The JSON reference protocol runs application traces through the interpreter and
compiled backend and compares:

- model snapshots;
- emitted command/subscription descriptions;
- template/slot output;
- public static state;
- errors.

The optimized patch protocol is then compared to the reference view result
through a DOM oracle. Optimization may change operation count, never final DOM,
focus/selection semantics, message order, effects, or errors.

## Performance contract

### Measure the complete path

The performance suite records:

- compiler cold and incremental time;
- development edit-to-browser latency;
- compressed/uncompressed HTML, CSS, loader, and Wasm bytes;
- Wasm download, compile, instantiate, and initialization time;
- first content and first interaction;
- update, view, slot comparison, boundary transfer, and DOM patch time;
- DOM operation count;
- peak and retained memory;
- long tasks;
- Core Web Vitals in representative deployments;
- server/static rendering throughput where applicable.

Results include hardware, browser, network/CPU shaping, build mode, sample size,
variance, and exact commit. A dashboard keeps historical results.

### Workload classes

No one workload decides the result. The suite includes:

1. **micro DOM:** the keyed and non-keyed js-framework-benchmark operations;
2. **forms:** typing, validation, IME, controlled values, and large form state;
3. **lists:** filtered/sorted data, stable keys, focus, and partial updates;
4. **dashboard:** timers, concurrent resources, charts through a presentation
   slot, and sustained updates;
5. **content:** the Witchy book as static HTML plus interactive regions lowered
   to resumable islands;
6. **cold mobile:** first visit under constrained mobile CPU/network;
7. **warm navigation:** cached assets and route transitions;
8. **memory endurance:** repeated navigation, mounting, and cancellation;
9. **server/static:** route rendering and artifact generation;
10. **adversarial:** large text, malformed events, rapid navigation, and stale
    completions.

Reference implementations use current stable versions of vanilla DOM, React,
Vue, Svelte, Solid, and one Rust/Wasm framework. They implement the same visible
behavior and safety-relevant escaping.

### Release thresholds

Phase 0 recorded the reference schema, local baseline, workload definitions,
and controlled-host procedure in `projects/glamour/PERFORMANCE.md`. The minimums
below are now normative. Release-relative timing claims remain unavailable until
the acceptance ledger contains clean records from the pinned macOS arm64 and
Linux x86-64 hosts with exact browser/framework versions. A steering review may
tighten but not weaken these minimums:

- no production event performs full-model JSON serialization;
- no unchanged static template structure crosses the Wasm boundary;
- one delegated listener exists per event class/root, independent of node
  count;
- a one-slot update emits no unrelated DOM operation;
- retained memory returns to within 5% of the post-start steady state after
  100 mount/unmount or route cycles, excluding documented caches;
- no Glamour workload is more than 2x the best non-vanilla reference for its
  primary update metric;
- the geometric mean of js-framework-benchmark CPU tests is within 1.5x of the
  best production framework in the comparison set;
- the content workload ships zero application Wasm/JavaScript on routes with
  no interactive regions;
- a minimal interactive application reports every loader/runtime byte and has
  a frozen, reviewed size budget before API stabilization;
- representative production deployments meet the "good" 75th-percentile Core
  Web Vitals thresholds: LCP at or below 2.5 seconds, INP at or below 200
  milliseconds, and CLS at or below 0.1;
- development median edit-to-visible-update latency is below 200 milliseconds
  for the starter and below 500 milliseconds for the Witchy book on the
  reference development machine.

Any exception needs a checked issue naming the workload, cause, owner, and
expiry. Aggregate scores cannot hide a catastrophic row.

### Performance invariants in CI

Functional tests assert structural performance facts such as operation count,
listener count, absence of JSON, allocation bounds, and cleanup. Noisy timing
benchmarks run on controlled hosts and block release promotion, not every
ordinary commit.

## Developer-experience contract

Performance is not enough. Before 1.0:

- a new Witchy user can create, run, test, and build the starter using only the
  documented commands;
- ten representative tasks are timed for first-time and experienced users;
- diagnostics are tested with deliberately broken examples;
- at least three external pilot applications are completed without private
  framework APIs;
- every pilot records setup problems, workarounds, missing primitives, and
  upgrade friction;
- no starter task requires selecting a router, state library, build tool, test
  runner, CSS processor, or deployment adapter;
- the generated production directory deploys to ordinary static hosting;
- the book includes a copy-paste path from install to deployed page;
- `witchy doctor --web` explains browser baseline, MIME/header requirements,
  capability policy, and artifact problems.

The 1.0 review publishes the task scripts and summarized results. A framework
cannot claim amazing developer experience based only on its authors' fluency.

## Implementation plan

Each phase lands independently behind versioned feature flags where needed.
Every phase updates an acceptance ledger with source, tests, benchmark evidence,
documentation, and known limitations. A later phase cannot redefine an earlier
phase's safety boundary silently.

### Phase 0 — freeze the reference and evidence

Deliver:

- specify the current JSON wire protocol as the semantic reference;
- capture current Glamour API, browser behavior, artifact size, operation
  counts, memory, and benchmark results;
- add a representative application corpus: counter, form, keyed list, resource
  dashboard, router, book, and compartment/secret example;
- add interpreter/compiled trace comparison;
- establish supported browser versions and controlled benchmark hosts;
- prototype `Site`, route, `Program`, and low-level `IslandPlan` declarations
  using existing Witchy functions, records, generics, traits, derives, and
  compile-time generation; this proves the lowering substrate without freezing
  an island-first public authoring API;
- inventory the exact stable-origin, generated-boundary, and target-availability
  gaps against that prototype;
- create the Glamour acceptance ledger and public performance dashboard format.

Exit criteria:

- every current behavior used by the book has a reference test;
- known current defects and limitations are recorded;
- benchmark commands reproduce from a clean clone;
- no Glamour-specific language keyword or annotation is proposed without a
  failed typed-library prototype and a named semantic gap;
- no optimization phase begins without a before measurement.

### Phase 1 — composition and complete application semantics

Deliver:

- `Program`, `Ui`, `Cmd`, and `Sub` public contracts;
- capability-free `view(model)` with authorization confined to initialization,
  updates, commands, and subscriptions;
- `Ui.map`, `Cmd.map`, and `Sub.map`;
- public `jsx` and `html` tagged-literal spellings over one checked parser;
- stable command/subscription identity and cancellation;
- compiler-generated typed callback ordinals and bounded private capture
  environments for optimized effects;
- compiler-authenticated root, route/resource, and structural effect scopes;
- typed `Resource`;
- declarative event decoders;
- root and feature lifecycle;
- deterministic scheduler and host simulator;
- migrate current builder APIs without changing the JSON host.

Exit criteria:

- nested features require no string message tags;
- stale Fetch/timer/subscription completions are tested;
- all new semantics have interpreter/compiled parity;
- current applications migrate mechanically or through a documented adapter.

### Phase 2 — typed templates, CSS, forms, routes, and accessibility

Deliver:

- static template-plan lowering for `html`;
- stable compiler-owned identities and source metadata for templates, slots,
  events, CSS, routes, and generated declarations;
- typed attributes, properties, URLs, events, branches, and keyed regions;
- sealed event-free `StaticUi` validation for fresh client-region fallbacks;
- sealed checked `MediaQuery` values produced by a static hole-free literal and
  shared by CSS, activation, and prefetch;
- `css` literals, extraction, scoping, assets, and typed class handles;
- typed route graph and static path generation;
- form schemas, host-custodied secret controls, and progressive form output;
- checked generated boundary declarations and target-availability diagnostics;
- static accessibility diagnostics;
- first accessible primitive set.

The host may still consume a JSON encoding of template plans during this phase.

Exit criteria:

- the starter, form, router, and book use the preferred APIs;
- no ordinary safe template needs generic string properties;
- CSS and route output are deterministic;
- formatting and unrelated edits do not perturb stable template, event, route,
  or island identities;
- server-only and browser-unavailable references fail at the source reference;
- accessibility fixtures pass the browser matrix;
- diagnostics point to source literal holes and declarations.

### Phase 3 — Wasm-resident model and binary patch protocol

Deliver:

- application instance and model ownership in Wasm;
- versioned checked event/patch/effect protocol;
- per-dispatch arenas and explicit buffer ownership;
- static template mounting and changed-slot patch emission;
- delegated events;
- move-minimizing keyed regions;
- JSON reference/optimized differential DOM oracle;
- protocol fuzzing and host hardening.

Exit criteria:

- production dispatch contains no model or VNode JSON;
- production effect descriptors and completions contain no callback, capability,
  probe value, or string-selected message variant;
- one-slot updates satisfy structural operation-count tests;
- malformed buffers fail closed;
- focus, selection, IME, autofill, and scroll tests pass;
- phase performance thresholds are met or have accepted expiring exceptions.

### Phase 4 — integrated development experience

RFC-0109 defines the command routing, deterministic artifact layout,
development server, safe hot-swap compatibility proof, diagnostics, developer
tools, production audit, and doctor contracts for this phase.

Deliver:

- `witchy new --web`, `witchy dev`, `witchy test`, and `witchy build --web`;
- incremental template/CSS/application compilation;
- state-compatible hot swap and explained fallback reload;
- source maps and browser error overlay;
- Glamour developer tools;
- `witchy doctor --web`;
- production build report, SBOM, capability summary, and header output.

The implemented development floor keeps source authority private and runtime
inspection read-only. The token-authenticated client fetches only the source map
whose content-derived build identity matches the running generation. That map
joins compiler template and slot wire identities to invocation and literal-hole
spans without embedding source text. It also records ordinary local Witchy
function spans and joins compiler-emitted Wasm names to function indices and
absolute body byte ranges; unmatched generated/toolchain functions receive no
invented source span. The optimized host retains at most 128 accepted frame
summaries with operation identities, payload byte lengths, and validation,
planning, DOM, host, and total timings. Descriptor summaries omit requests and
results. Model fields expose only a compiler-owned scalar or aggregate category
while every value remains `"<redacted>"`. Production Wasm and bundles omit the
metadata and bridge.
An independent 128-entry host timeline records only effect/subscription kind,
phase, numeric instance, descriptor, generation, completion status, and an
optional closed semantic category from the authenticated descriptor. The
accepted categories are `resource`, `navigation`, `timer`, `port`, `storage`,
`worker`, and `custom`; unknown values are omitted. It does not retain request
or result values, and diagnostic observation cannot alter host work.
Development codegen compares each next model field inside Wasm and exports only
a checked one-byte change bitmap. Scalars compare directly; aggregates compare
their compiler-known bounded equality shape, including nested string and list
content. Frame records contain changed field indices; values and aggregate
shapes never enter JavaScript and remain `"<redacted>"` in inspection. Scalar
snapshot format 1 retains its fixed-width authenticated hot swap. A model with
aggregate fields receives format 2 only when its complete recursive shape has a
sealed `PublicState` proof. The compiler generates an exact typed encoder and
decoder over the closed `IslandCapture` tree, bounds the deterministic wire and
complete snapshot to 1 MiB, authenticates the recursive nominal/variant/field
schema, and deep-copies decoded values into the candidate's stable arena. Raw
Wasm pointers and private representation bytes never cross builds. A capability,
`Bytes`, function, dynamic value, sealed handle, or nominal without
`derive(PublicState)` keeps format 0 and exports no snapshot/restore authority.

A build may accept one previous aggregate schema by defining the ordinary typed
function `fn glamour_migrate(previous: PreviousModel) -> CurrentModel`.
`PreviousModel` must itself have a sealed recursive `PublicState` proof. The
compiler generates the previous-schema decoder, calls the checked migration,
and advertises that exact schema in the private development manifest. The dev
server and browser select swap only when the candidate names the running schema;
authorization, template, application, snapshot-format, and byte-limit identities
must still match. Restore runs in a detached candidate. Header, bound, format,
and schema rejection happens before live state exists; malformed typed payloads
abort only the candidate. The old application is disposed after decode,
migration, stable-arena copy, initial emit, and host authentication all succeed.
The read-only application summary records whether the root was freshly mounted,
resumed, or restored by authenticated hot swap, together with live node, region,
and listener counts. It exposes neither DOM objects nor the dispatch-capable
application. When explicitly enabled for development, the island scheduler
separately exposes a fresh frozen hierarchy of its manifest-bounded checked
instances: public instance/artifact/key and parent identities, activation policy,
status, queued-event count, and authenticated resume-versus-fresh outcome. It
exposes no public-state JSON, DOM/application object, event payload, or dispatch
handle.
Each named Wasm body also reports its exact decoded instruction boundaries as
absolute module byte offsets. The private map deliberately omits operator names
and immediates. Ordinary source declarations include `impl` methods; mapping
requires an exact compiler mangle or an unambiguous type-and-method suffix.
Compiler-owned development WIR wrappers map each source statement root to its
exact emitted instruction-ordinal and absolute byte interval without changing
the Wasm bytes or embedding the sidecar in production. The loaded-source parser
also inventories sorted, deduplicated nested expression-root ranges with exact
line/column starts and exclusive ends for ordinary functions and `impl` methods.
Each expression retains its parser-owned containing statement line. When that
line has one unambiguous compiler statement mapping, the private map attaches
the statement's exact instruction and byte interval as containment evidence.
Expressions intentionally carry no narrower byte interval until their identity
survives linking and lowering. Exact sub-statement Wasm intervals, annotated
public values, compiler-emitted route and resource semantics, replayable
results, and time travel remain required for the complete tools described above.

Client-mode invalidation also follows the exact local entry/import graph used by
the checked development build. Unimported `.witchy` siblings do not perturb the
compiler fingerprint; changing a loaded module invalidates only the compiler
artifact class, while template, style, and public assets keep their independent
fingerprints. The current compiler still relinks, checks, and emits the complete
loaded unit after that invalidation. Per-module checked IR and codegen reuse,
reverse-dependent invalidation, and controlled incremental timing thresholds
remain required before claiming an incremental compiler.

Exit criteria:

- clean-clone onboarding and broken-example diagnostic studies pass;
- the maintained path needs no Node/npm installation;
- hot reload cannot preserve an incompatible model silently;
- production builds contain no development authority or debug exports by
  default.

### Phase 5 — static rendering and progressive applications

Deliver:

- native template renderer;
- typed `Site` manifest and route entry contract expressed with ordinary Witchy
  values;
- deterministic route/static artifact generation;
- zero-runtime static pages;
- progressive links and forms;
- critical CSS and asset/preload graph;
- portable optional Witchy server adapter.

Exit criteria:

- the non-runnable book routes ship no application runtime;
- static HTML matches the browser DOM oracle;
- server and client request/form decoders agree;
- the output deploys unchanged to GitHub Pages and another ordinary static
  host;
- production Core Web Vitals meet the release thresholds on the reference
  deployment.

### Phase 6 — interactive regions and resumable island delivery

Deliver:

- typed `glamour.interactive` declarations with one program view, a sealed
  `Interactive` authoring value, and compiler-owned `IslandPlan` lowering;
- typed `glamour.client_region` for checked inert fallback plus an explicitly
  fresh browser start when no public initial model exists;
- split capability-free `initial(Start)` model construction from authorized
  `start(auth, model)` work so fresh and resumed activation cannot lose startup
  commands;
- advanced activation, prefetch, and diagnostic-name controls with no
  Glamour-specific language keyword;
- `PublicState` derivation and rejection rules;
- build/template/event identity in static HTML;
- activation without initial-template replay;
- island-granular Wasm loading and shared-module grouping;
- resume mismatch fallback;
- per-island capability grants, lifecycle, debugging, and memory cleanup.
- build-authenticated per-island grant specialization and browser-policy
  derivation;
- a no-overtake barrier between an interaction that promotes activation and any
  startup completion or initial subscription emission.

Exit criteria:

- the ordinary interactive-region example contains no `IslandPlan`, island
  manifest, second `static_view`, or manual cross-island state wiring;
- static output and resumed updates derive from the same `view(model)`;
- `client_region` is labeled fresh, serializes no model, calls `initial`
  exactly once, and calls `start` exactly once after authorization;
- interaction-activated islands with `NoPrefetch` fetch or execute no
  application code before interaction; prefetched islands execute none;
- heterogeneous interactive regions lower through one closed manifest type
  without stringly typed program, model, or message selection;
- heterogeneous constructors and repeated placements join through authenticated
  structural origins, never evaluation or registry order;
- a resumable region's triggering event is neither lost nor duplicated;
- a prevented native navigation or submission either reaches the activated
  program or executes its authenticated progressive fallback exactly once;
- startup work cannot overtake the interaction that promoted activation;
- a fresh event-free fallback treats its first interaction as activation only,
  preserves native browser behavior, and never forges a post-mount message;
- capabilities, secrets, and host handles cannot enter resumable state;
- independently activated islands cannot address each other's nodes or state;
- independently activated islands cannot use a descriptor outside their
  build-authenticated instance grant;
- features requiring frequent shared state compose into one `Program`; no
  implicit cross-island mutable store exists;
- resumption and controlled DOM-rebuild fallback from the same authenticated
  public model produce equivalent live state and output;
- cold-mobile evidence shows an improvement over whole-page client boot for
  the book and at least one pilot application.

### Phase 7 — hardening, pilots, and 1.0

Deliver:

- complete browser, security, accessibility, and endurance matrices;
- three external pilot applications;
- API migration tooling from the compatibility surface;
- framework-relative benchmark implementations and audited results;
- versioning, support, deprecation, and security-response policies;
- stable documentation and component catalog;
- final removal of the production JSON render path;
- 1.0 release candidate and independent adversarial review.

Exit criteria:

- all prior phase ledgers are green;
- no unresolved critical/high security finding exists;
- no known interpreter/compiled semantic divergence exists;
- every public capability and host extension has threat-model documentation;
- accessibility manual review is complete;
- performance and developer-experience contracts pass;
- the book and pilots use only public 1.0 APIs;
- compatibility APIs have a documented support window;
- the release candidate passes the exact serialized release gate.

## Migration and compatibility

### Current applications

The current `VNode`, `Attr`, `Cmd`, `step_with`, and JSON host remain available
under a `glamour.compat` module during the migration window.

Migration proceeds in this order:

1. replace string message tags with typed event decoders;
2. introduce `Program`, remove authority from `view`, and project visible
   permission facts into ordinary model data;
3. adopt mapped feature composition;
4. move generic properties and URLs to typed template slots;
5. adopt `Sub` and `Resource` for ongoing/async work;
6. adopt typed routes, forms, and CSS;
7. compile templates to the optimized host;
8. select static, embedded-interactive, or client delivery per root; advanced
   island controls remain optional.

`witchy glamour migrate` reports transformations and leaves explicit TODOs
where behavior is ambiguous. It never rewrites secret or capability boundaries
without review.

### Host API

The current `glamour-dom.mjs` mount contract remains versioned. The optimized
host is a new protocol version and artifact. A page cannot accidentally pair an
old host with a new application: build identity and protocol negotiation fail
before mount.

### Deprecation

Compatibility APIs receive at least one documented minor-release migration
window after 1.0. Security fixes may narrow unsafe behavior sooner, with a
specific advisory and migration.

## Governance and evidence

This umbrella RFC owns direction and phase boundaries. Focused follow-up RFCs
are required for changes that introduce:

- new Witchy language syntax;
- a stable public compile-time origin/metadata contract;
- new capability kinds;
- privileged host slots;
- a public binary ABI;
- target-availability or static/server/browser placement rules;
- serialization derivations;
- browser baseline changes;
- a stable package/distribution contract.

The acceptance ledger distinguishes:

- `PROVEN` — implemented and supported by checked evidence;
- `MISSING` — owned by this RFC but not implemented;
- `FAILING` — implemented behavior does not meet the criterion;
- `EXTERNALLY OWNED` — blocked by a named prerequisite outside this RFC;
- `DEFERRED` — explicitly removed from the 1.0 scope by a follow-up decision.

Marketing claims use only `PROVEN` entries. "Safe by construction,"
"resumable," "zero runtime," and performance comparisons each name the exact
scope in which the evidence applies.

## Alternatives

### Adopt React's component and Hooks model

This would improve familiarity and ecosystem interop, but it would discard
Glamour's clearest advantage: state transitions and effects are explicit
values. It would also import call-order rules, closure/dependency concerns, and
a component rerender lifecycle that the compiler would then need to optimize
away.

Rejected as the semantic model. React interop can exist in an isolated host
slot or compartment when needed.

### Expose signals as the primary state API

Signals can produce excellent fine-grained performance and are converging as
framework infrastructure. A mutable graph exposed throughout application code
would, however, weaken replay, make authority/effect boundaries easier to blur,
and create two state models beside ordinary Witchy values.

Rejected as the primary API. Signal-like dependency tracking remains available
inside the compiler/runtime implementation and may later be exposed as a
specialized, pure derived-value facility if evidence requires it.

### Keep a conventional virtual DOM

A VDOM is simple, general, and useful as a reference. The current JSON VDOM also
makes cross-backend inspection easy. It performs unnecessary allocation,
serialization, tree walking, and host-side reconciliation for templates whose
structure the compiler already knows.

Retained as a compatibility/reference path and dynamic fallback; rejected as
the optimized production path.

### Direct DOM bindings from Wasm

Giving application code imported DOM functions could remove the patch buffer.
It would create a wide, chatty Wasm/JavaScript boundary, expose ambient mutable
objects, weaken the host validator, and make static rendering and parity harder.

Rejected. A compact batch protocol keeps the boundary narrow and auditable.

### Whole-page hydration

Hydration is widely understood and simpler than resumption. It downloads and
replays code to rediscover behavior that the static renderer already knew.

Supported only as controlled fresh-mount fallback. Static pages and
compiler-lowered interactive regions are the preferred content path.

### Make islands the primary component model

Explicit component islands are effective for content-oriented pages and make
delivery boundaries easy to see. They become awkward when application regions
share state, navigation, keyboard behavior, or coordinated updates. Making
authors choose an island boundary for every reusable feature would leak Wasm
packaging and activation concerns into ordinary program design.

Rejected. `Program` and typed feature mapping are the composition model.
`Interactive` marks the rarer independently activatable delivery boundary, and
the compiler lowers it to an island. Chatty regions remain one program.

### Server-only rendering like LiveView

A server-owned state machine can provide an excellent integrated experience and
small client runtime. It adds network latency to interaction, requires a live
connection and server memory, complicates offline behavior, and turns hosting
into part of the framework contract.

Glamour server mode may support server-driven features later, but the core
remains deployable as static files or a client application.

### Depend on Vite/npm for the web toolchain

This would provide mature HMR and plugin ecosystems quickly. It would add a
second package manager, lockfile, dependency graph, configuration model, and
supply-chain surface to the maintained path.

Rejected for the default. Explicit external-tool integrations may exist, but
the starter and 1.0 guarantees use the Witchy toolchain.

### Add an `island` keyword or built-in Glamour/JSX declaration grammar

A dedicated declaration can make a demo shorter, but the author-facing contract
is already `glamour.interactive(program, initial)`. The compiler-owned island
plan combines that checked program, activation policy, public-state codec, and
template metadata. Existing Witchy functions, records, generics, derives, and
compile-time generation can express the authoring contract and keep it
inspectable with ordinary tools.

Rejected for 1.0. The typed `Interactive` API and its `IslandPlan` lowering must
be piloted first. Any later sugar must solve a demonstrated general language
problem, expand to the library form, and receive its own RFC.

This does not reject `jsx"..."` or `view"..."` as tagged literals. Those are
libraries using RFC-0006's existing generic compile-time syntax, just as
`html`, `sql`, and `css` are.

### Only improve the current library

Adding helpers without changing the transport would make source code nicer but
leave the fundamental performance, static delivery, and tooling ceilings in
place.

Rejected. The work is staged so ergonomic improvements land early, but the 1.0
claim requires the full architecture.

## Drawbacks and risks

### Scope

This is a large product program spanning compiler, runtime, web host, standard
library, CLI, static renderer, testing, documentation, and ecosystem work.
Phased ledgers and independently useful exits limit the risk, but they do not
make the work small.

### Compiler complexity

Static template lowering, source identity, typed CSS, route generation, and
slot optimization add compiler surface. The reference VDOM/JSON path and
differential oracle are required because optimized DOM output is difficult to
validate from unit tests alone.

### Wasm startup and size

Wasm is not automatically faster than JavaScript. Download, compilation,
instantiation, boundary crossing, code size, and memory can erase compute
advantages, especially for small pages. Static output and activation-based
delivery are therefore architectural requirements, not optional polish. The
runtime needs resumable islands; ordinary source does not need an island-first
architecture.

### Resumability constraints

Resumption requires stable identities and serializable public state. Some
applications will need client-only initialization or server-owned state. The
first Wasm unit remains an island, so a poorly chosen interactive boundary may
load more code than Qwik-style symbol splitting. `OnInteraction` can also turn
network and compilation time into first-input delay. The default `OnVisible`,
separate prefetch controls, event buffering, artifact grouping, and build
reports mitigate these costs. The framework must report them rather than
presenting every page as resumable or every deferred activation as better UX.

### Delivery abstraction drift

`Interactive`, `IslandPlan`, the runtime instance, and an application feature
are distinct concepts. Tooling and documentation can accidentally collapse
them back into one word and force authors to reason about compiler internals.
The starter and first learning path use only `Program` and `Interactive`;
`IslandPlan` appears only in advanced delivery, diagnostics, and implementation
documentation. API review rejects convenience methods that create implicit
cross-island state or lifecycle semantics.

### Ecosystem size

Glamour cannot initially match React's packages, hiring pool, integrations, or
browser battle history. A small, coherent maintained surface and explicit
ports/slots/compartments are the response. Compatibility marketing must not
pretend the gap is absent.

### Integrated defaults can become a cage

One router, form model, and CSS path reduce choice overload but can frustrate
advanced applications. Each subsystem therefore exposes typed lower-level
primitives and narrow host extension points. Alternatives must preserve the
same capability and sink safety contracts.

### Benchmark gaming

Public thresholds can encourage special cases. Workload diversity, operation
invariants, exact-source reference implementations, aggregate and worst-row
reporting, and independent review reduce that risk.

### API instability before 1.0

The architecture will learn from pilots. The project must label experimental
APIs honestly, provide migration tools, and resist freezing accidental
prototype shapes merely to avoid a controlled pre-1.0 change.

## Prior art and research

Research was reviewed on 2026-08-01. Framework behavior comes from maintained
project documentation; sentiment comes from the named 2024/2025 surveys.
Survey rankings will age, so they motivate the problem statement but do not
define the architecture or its release gates.

### Developer sentiment

- [State of JavaScript 2024: front-end
  frameworks](https://2024.stateofjs.com/en-US/libraries/front-end-frameworks/)
  — Svelte led positive opinion; respondents identified React-specific issues,
  complexity, performance, choice overload, breaking changes, state
  management, change velocity, dependencies, and SSR.
- [State of JavaScript 2025: front-end
  frameworks](https://2025.stateofjs.com/en-US/libraries/front-end-frameworks/)
  — Solid had the highest satisfaction for five consecutive years; recurring
  pain points remained complexity, performance, state management, choice,
  breaking changes, browser support, dependencies, and bloat.
- [State of JavaScript 2024:
  meta-frameworks](https://2024.stateofjs.com/en-US/libraries/meta-frameworks/)
  — Astro and SvelteKit retained high satisfaction while meta-framework pain
  centered on complexity, breakage, SSR, performance, documentation,
  deployment, and integration.
- [Stack Overflow 2025 technology
  survey](https://survey.stackoverflow.co/2025/technology/) — Phoenix was the
  most admired web framework at 79%, and had held that position since 2023.

Surveys are directional and have respondent-selection effects. This RFC uses
them to identify recurring problems, not to prove that one architecture wins.

### Framework comparison

| System | What it demonstrates | Glamour takes | Glamour does not take |
|---|---|---|---|
| [React](https://react.dev/reference/react) | A powerful component ecosystem, declarative views, scheduling, server rendering, and a large body of production practice | One-way data flow, declarative UI, keys, error boundaries, source-level composition | Hooks, dependency arrays, effect-driven application logic, component rerendering as the optimization boundary |
| [Vue](https://vuejs.org/guide/extras/composition-api-faq) | Automatic reactive dependency collection and strong single-file authoring ergonomics | Compiler/runtime dependency knowledge and low-ceremony composition | Mutable proxy state as the application semantic core |
| [Angular signals](https://angular.dev/guide/signals) | Fine-grained consumer tracking can modernize a large established framework | Granular invalidation and explicit derived values inside the optimized implementation | A broad application container, decorators, or dependency injection as the default shape |
| [Svelte](https://svelte.dev/docs/svelte/what-are-runes) | Compiler-aware syntax can turn ordinary-looking source into narrow updates and scoped assets | Compile-time templates, checked CSS, direct updates, excellent source diagnostics | JavaScript mutation semantics or framework keywords for hidden effects |
| [Solid](https://docs.solidjs.com/concepts/signals) | A fine-grained graph can update exact consumers without a VDOM | Slot-level update work and cached pure derivations | Requiring authors to manually distribute mutable signals through domain code |
| [Astro](https://docs.astro.build/en/concepts/islands/) | Static HTML can be the default and interactivity can be an explicit island | Zero-runtime static routes, activation policies, independent islands | A multi-framework JavaScript integration layer as the core product |
| [Qwik](https://qwik.dev/docs/concepts/resumable/) | Listener, structure, and state identity can be serialized for resumption | Stable identities, no template replay on resume, event delegation, serialization checks | Pretending arbitrary heaps serialize, or promising function-level Wasm splitting before evidence |
| [Marko](https://markojs.com/docs/explanation/why-is-marko-fast) | Targeted compilation can resume server output and emit browser code only for stateful values, handlers, and effects rather than whole component islands | One authoring model, compiler-selected static work, and expression-level delivery as the long-term direction | Requiring JavaScript mutation or making authors maintain parallel server and client templates |
| [Elm](https://guide.elm-lang.org/architecture/) | Explicit models, messages, updates, commands, and subscriptions support local reasoning and testing | The application semantics, enhanced with Witchy capabilities and compiler templates | Requiring hand-written parent wiring where safe compiler derivation can remove it |
| [Phoenix LiveView](https://hexdocs.pm/phoenix_live_view/) | An integrated event model, server rendering, and excellent operational tooling can produce unusually high satisfaction | Coherent defaults, explicit events, typed server integration, strong diagnostics | A mandatory live server/connection for interaction |
| [htmx](https://htmx.org/docs/) | Native HTML and hypermedia can solve many interactions with very little client code | Progressive forms/links and server responses that remain useful before activation | String attributes that implicitly acquire network or DOM authority |
| [Lit](https://lit.dev/docs/) and Web Components | Standards-based custom elements provide durable browser interoperation | Narrow host elements/slots and platform-native semantics | Making mutable custom-element instances Glamour's state owner |
| [Leptos](https://book.leptos.dev/reactivity/index.html) and [Dioxus](https://dioxuslabs.com/learn/0.7/guides/platforms/) | Rust/Wasm frameworks demonstrate serious typed browser applications and expose Wasm startup, interop, and artifact tradeoffs | Wasm-resident state, typed source, platform portability, explicit measurement | Assuming Wasm is faster merely because computation is compiled |

This comparison is a design input, not a feature checklist. Glamour's coherent
combination is the differentiator: explicit state/effects, compiler-generated
fine-grained DOM work, static/resumable delivery, capability authority, and one
toolchain.

### Explicit state and effects

- [The Elm Architecture](https://guide.elm-lang.org/architecture/) and
  [commands/subscriptions](https://guide.elm-lang.org/effects/) establish the
  model/update/view and described-effects foundation that Glamour extends with
  capability authority and compiler templates.
- [Phoenix LiveView](https://hexdocs.pm/phoenix_live_view/) demonstrates an
  integrated event-driven model, excellent server rendering, and small client
  coordination, with different server/network tradeoffs.

### Fine-grained and compiler-directed UI

- [Solid signals](https://docs.solidjs.com/concepts/signals) demonstrate
  fine-grained dependency tracking and direct consumer updates.
- [Svelte runes](https://svelte.dev/docs/svelte/what-are-runes) and
  [`$state`](https://svelte.dev/docs/svelte/%24state) demonstrate
  compiler-controlled reactive syntax and granular updates with ordinary-looking
  values.
- [Vue's Composition API comparison with React
  Hooks](https://vuejs.org/guide/extras/composition-api-faq#comparison-with-react-hooks)
  documents call-order sensitivity, stale closures, dependency arrays, and
  manual memoization, and contrasts them with automatic dependency collection.
- [React: You Might Not Need an
  Effect](https://react.dev/learn/you-might-not-need-an-effect) treats Effects
  as an external-system escape hatch and documents the correctness and extra
  render costs of using them for derived state or event logic.
- The [TC39 Signals proposal](https://github.com/tc39/proposal-signals) records
  cross-framework convergence on a lazy, cached, glitch-free underlying signal
  graph while explicitly targeting framework infrastructure rather than a
  common application-facing API.

### Static, islands, and resumption

- [Astro's islands
  architecture](https://docs.astro.build/en/concepts/islands/) renders static
  HTML by default and loads client JavaScript only for explicitly marked
  interactive islands.
- [Qwik resumability](https://qwik.dev/docs/concepts/resumable/) serializes
  listener, component, and state information to avoid whole-page hydration,
  while documenting the serializability constraints that this requires.
- [Marko targeted
  compilation](https://markojs.com/docs/explanation/targeted-compilation) emits
  different server and browser programs from one template and includes browser
  code only for interactive work. Its current documentation explicitly
  contrasts expression-level analysis with older component-island boundaries.

Glamour adopts Astro's static-first delivery boundary, Qwik's no-replay
resumption goal, and Marko's separation between simple authoring and
compiler-selected client work. Authors embed `Program` values as `Interactive`
regions. The compiler lowers those regions to authenticated resumable islands.
The first lazy runtime unit is a Wasm island, not a JavaScript function symbol;
finer compiler-selected splitting remains a measured future optimization.

### WebAssembly delivery and interfaces

- [`WebAssembly.instantiateStreaming`](https://developer.mozilla.org/en-US/docs/WebAssembly/Reference/JavaScript_interface/instantiateStreaming_static)
  is the efficient browser loading path and requires correct hosting/CSP.
- The [WebAssembly Component
  Model](https://component-model.bytecodealliance.org/design/components.html)
  demonstrates typed, versioned interfaces and a canonical ABI for richer
  cross-component values. Glamour's browser protocol is smaller and
  DOM-specific but follows the same principle: name the interface and ownership
  contract rather than exchanging ad hoc JSON forever.

### Performance, security, and accessibility standards

- [js-framework-benchmark](https://github.com/krausest/js-framework-benchmark)
  provides useful repeatable keyed/non-keyed DOM microbenchmarks, but this RFC
  treats it as one workload class.
- [Core Web Vitals
  thresholds](https://web.dev/articles/defining-core-web-vitals-thresholds)
  provide user-centered production targets for LCP, INP, and CLS at the 75th
  percentile.
- [Trusted Types](https://developer.mozilla.org/en-US/docs/Web/API/Trusted_Types_API)
  and [Content Security
  Policy](https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/CSP) provide
  browser-enforced boundaries around injection and resource authority.
- [WAI-ARIA Authoring
  Practices](https://www.w3.org/WAI/ARIA/apg/) defines accessible widget
  patterns and keyboard interactions; Glamour prefers native semantics and uses
  these practices for composite widgets.

## Final acceptance

Glamour 1.0 may be called the best, safest, or most performant system only with
a scope attached:

- **safest** means the default template/effect/host path has the documented
  capability, sink, CSP, Trusted Types, secret-custody, fuzzing, and
  cross-island evidence;
- **most performant** means the named workloads, browsers, hardware, artifact
  sizes, memory results, and production vitals beat or meet the published
  comparison under reproducible conditions;
- **best developer experience** means the documented onboarding/task studies,
  pilot results, diagnostics, integrated defaults, and migration evidence pass.

The architectural target is ambitious by design. The release language remains
precise. Glamour wins by making correct programs beautiful to write, unsafe
programs difficult to express, and runtime work proportional to what actually
changed.

---
rfc: 0103
title: "Derived platform confinement: kernel and web enforcement of capability grants"
status: accepted
created: 2026-07-23
tracking: >
  Reopened for full implementation. Phase 1 is implemented: native Dir, File,
  build-root, and executable selection authority is descriptor/handle anchored
  through one shared confine implementation used by both backends, with
  deterministic parent-swap regressions. The target-neutral
  `witchy-confinement` policy model and the single concrete-grant derivation
  boundary are implemented. Its Linux provider safely translates filesystem
  and TCP policy into independently reported Landlock rulesets. Launch
  activation, required launch mode, and seccomp promise classes are
  implemented. Build-step outer confinement, derived CSP, and enforcement
  menus remain active implementation phases.
predecessors:
  - "[0013](0013-capability-grant-documents.md) (grant documents — the concrete pre-execution authority statement this RFC compiles)"
  - "[0068](0068-compiled-build-step-grants.md) (build-step grants — the declared authority of third-party build code)"
  - "[0092](0092-trusted-application-executables.md) (trusted executables — the embedded binding plan the launcher can enforce)"
  - "[0102](0102-portable-roots-and-the-fetch-capability.md) (provider doctrine and host menus — this RFC is its enforcement mirror)"
related:
  - "[0020](0020-rebinding-resistant-http.md) (rebinding-resistant HTTP — cites cap-std/Capsicum; the pinned-dial machinery kernel policy backs up)"
  - "[0091](0091-browser-virtual-capabilities.md) (browser host — the web platform whose CSP analog this RFC formalizes)"
---

# RFC-0103: Derived platform confinement — kernel and web enforcement of capability grants

> Provisional syntax. Code blocks here are intentionally **not** tagged
> `witchy` so the doc-examples test does not try to compile them.

## Summary

witchy knows, **before a program executes**, the exact external authority it
may use: the grant set — `Dir` roots with rights, `File` paths, `Net`
addresses and ports (and `Fetch` origins, RFC-0102), `Exec` program
allowlists, named secrets. Today one layer enforces that knowledge: the
runtime host (the wasm kernel's import handlers plus `confine`'s path
checks). This RFC adds a second, independent enforcement layer **derived
mechanically from the same grants** and applied at the platform boundary,
outside the runtime's own trust base:

- **Linux**: a Landlock ruleset (filesystem subtrees + rights, TCP
  connect/bind ports) and a grant-conditional seccomp filter, applied after
  grant resolution and before guest execution;
- **native hosts, per-operation**: race-free descriptor/handle-anchored path
  resolution through `cap-std` (the platform-specific `openat`/`openat2` or
  handle walk) replacing `confine`'s canonicalize-then-use;
- **the web**: a Content-Security-Policy derived from the capability
  surface (`connect-src` = the granted `Fetch` origins), formalizing and
  extending the hardened header suite coven-web already ships;
- **later, per owner**: OpenBSD `pledge`/`unveil`, macOS Seatbelt, FreeBSD
  Capsicum — **confinement providers**, one per platform, mirroring
  RFC-0102's capability providers.

The layer is defense in depth, not semantics: it contains bugs in the
runtime kernel, wasmtime escapes, and dependency vulnerabilities. Its
soundness invariant is that the derived policy is always a **strict
superset** of granted authority — the outer fence never denies a legitimate
granted operation, so program-observable behavior (and twin-backend parity)
is unchanged by construction, and any outer-fence denial is a contained bug,
never a program error.

## Motivation

### The enforcement stack has one load-bearing layer

A native run trusts: the compiler (capability typing, footprint), the
runtime kernel (grant admission, import handlers, `confine`), and wasmtime.
A single bug in any of these yields the process's full ambient authority —
every file the user can read, arbitrary network, arbitrary exec.
`confine.rs` documented a live instance of the risk class. Phase 1 has now
removed that canonicalize-then-open model: roots are admitted as open directory
handles and all guest-selected components are resolved beneath those handles.
Retained `File` and subtree capabilities remain attached to their original
parent objects even if their old ambient names are replaced.
The wasm sandbox is strong, but "strong" is not "assumed infallible" — every
serious sandbox architecture (Chrome's, OpenBSD's, systemd's) layers an
outer kernel fence precisely because inner layers have bugs.

### witchy is unusually well-positioned to derive that fence

Most sandboxing efforts start by *discovering* what a program needs (strace
audits, permissive-then-tighten). witchy starts with the answer: the grant
set is explicit, resolved, and machine-readable at launch — grant documents
(RFC-0013), CLI flags, the trusted-exe binding plan (RFC-0092), build-step
grants (RFC-0068). `Dir[Read]` *is* a Landlock read rule; a program with no
`Exec` grant *is* a seccomp filter with `execve` denied; a `Fetch` origin
allowlist *is* a CSP `connect-src`. The policy compiler is a mapping, not an
inference.

### The highest-value surfaces are the least-trusted code

- **Build steps** run third-party code by construction with declared grants
  — the sharpest supply-chain surface in the system. Kernel-enforcing
  `[build.grants]` means a compromised build dependency is confined even if
  the package-manager layer mis-checks a widening.
- **Trusted executables** carry their binding plan inside the artifact. The
  launcher can apply kernel policy before *any* guest code runs — installed
  apps get kernel-level least privilege with zero user configuration, making
  the RFC-0092 trust statement ("trusts the application, its runtime, and
  its distributor") strictly weaker than today.
- **The playground** executes arbitrary user-typed code in a page; CSP is
  the platform fence when the wasm sandbox is the thing being probed.

## Doctrine

### Confinement providers mirror capability providers

RFC-0102 made capability types universal with per-host providers. This RFC
is the enforcement mirror: one shared **policy derivation** (grants →
abstract confinement policy: filesystem subtree/rights pairs, port sets,
syscall classes, origin sets) with per-platform **confinement providers**
that translate it (Landlock+seccomp, pledge/unveil, Seatbelt, Capsicum,
CSP). A host's menu (RFC-0102) advertises its enforcement:

```
# menus/native-linux.toml (excerpt)
[enforcement]
fs      = "landlock"        # ABI >= 1; probed at launch
net     = "landlock-tcp"    # ABI >= 4: connect/bind by port
syscall = "seccomp-classes"
per-op  = "openat2-beneath"
```

so `witchy caps --menu` can show not just what a host grants but how many
independent layers enforce it.

### The superset invariant (soundness direction)

The derived policy must permit **at least** everything the grants permit,
including the runtime's own operational needs (module cache, DNS files when
network is granted, JIT memory transitions). An outer-fence denial of a
legitimately granted operation would surface as a host error the other
backend does not produce — a parity break. Therefore: derive generously,
tighten with evidence, and treat any outer-fence denial in testing as a
derivation bug. The converse direction is the security property: everything
*outside* the grants that the platform can express is denied.

### Best-effort by default, required on demand

Platform enforcement depends on kernel/browser versions (Landlock ABI
probing, CSP feature support). Because the layer is semantically invisible,
best-effort application preserves correctness on every platform; a
`--confine=required` mode (and a trusted-exe manifest flag) refuses to run
where the platform cannot enforce, for deployments that demand the fence.
Degradation is reported, never silent: the host prints which layers armed.

### Additive and monotone

Landlock rulesets stack, seccomp filters intersect, `pledge` only shrinks,
CSP only tightens — every provider is restriction-only, matching the
narrowing doctrine. Applying the fence can never widen anything, so the
order (resolve grants → arm fence → run guest) is safe by construction.

## The derivation

### Filesystem (Landlock; unveil/Seatbelt/Capsicum analogs)

| Grant | Landlock rule |
|---|---|
| `Dir[Read]` root | `READ_FILE \| READ_DIR` on the root's tree |
| `Dir[Read, Write]` root | adds `WRITE_FILE \| CREATE_* \| REMOVE_* \| TRUNCATE` |
| `File[Read]` / `File[Write]` path | per-file rule on the parent + name |
| runtime needs | module/source cache dirs (read/write), `/etc/resolv.conf`, `/etc/hosts`, TLS roots — unioned in when network is granted |

Pre-opened descriptors (stdout/stderr, inherited pipes) are unaffected by
Landlock — the conventional `Console` path needs no rule.

### Network

- **Landlock ABI ≥ 4**: TCP `connect`/`bind` restricted to the granted port
  set (`Net[Connect]` addresses and `Fetch` origins contribute connect
  ports; `Net[Listen]` contributes bind ports). Port-granular only — the
  host layer's address allowlist and pinned dials (RFC-0020) remain the
  primary, finer-grained enforcement; the kernel layer bounds the blast
  radius of a host-layer bug.
- **No network grant at all**: the seccomp class denies the socket-family
  syscalls outright — categorically stronger than any allowlist.

### Syscalls (seccomp): promise classes, not a hand-audited list

The maintenance scar of raw seccomp is the full-allowlist that breaks on
every toolchain upgrade. Adopt `pledge`'s lesson instead: witchy defines a
small set of **promise classes** and derives which are armed from the
grants:

| Class | Contents (representative) | Armed when |
|---|---|---|
| `base` | mmap/mprotect (JIT), futex, epoll, read/write on open fds, clock, getrandom, exit | always |
| `fs-open` | openat/openat2/stat family | any `Dir`/`File` grant (or runtime cache) |
| `net` | socket/connect/sendto/recvfrom/getaddrinfo support set | any `Net`/`Fetch` grant |
| `listen` | bind/listen/accept | `Net[Listen]` |
| `proc` | clone/execve/wait/pipe | `Exec` grant |

Deny-by-default over classes; the class contents are maintained once,
centrally, with a CI canary that runs the full differential suite under the
armed filter (any outer-fence denial fails the derivation, per the superset
invariant).

### Exec children: the declared hole

Landlock rules and `no_new_privs` (required by unprivileged seccomp)
inherit across `execve`. That inheritance is a semantic change worth
wanting: today an `Exec` child runs with the user's full ambient authority —
the widest hole in the fence. Under this RFC:

- **Default: children inherit the confinement.** A `git` invoked by a
  program granted only `Dir[Read] ./repo` sees only that subtree — secure,
  and sometimes breaking (tools read `~/.gitconfig`, `/etc/…`).
- **Widening is declared, not ambient.** The `Exec` grant vocabulary gains
  optional child-needs (`exec = { programs = ["git"], child-paths =
  ["~/.gitconfig"] }` in grant documents, the trusted-exe exec binding, and
  `[build.grants]`). The fence stays kernel-enforced; the hole becomes an
  explicit, auditable declaration.

This is deliberately the same shape as the rest of the capability system:
authority is visible at a declaration site or it does not exist.

### Environment and secrets

No kernel analog: the environment is materialized into the process before
confinement and secrets are read by the host. These stay host-layer
(RFC-0102's Env granted-name repair is the enforcement there). Honest gap,
recorded.

### Granularity limits (recorded honestly)

Kernel policy is **per-process**: a process hosting several VMs is confined
to the union of their grants; per-guest granularity remains the host
layer's job. Landlock networking is port-granular, not address-granular.
The kernel layer is a *bound*, not a replacement — the host layer remains
the primary and finer enforcer everywhere.

## Per-operation confinement (phase 1, independent of policy)

**Implemented:** `ConfinedDir` consumes ambient authority only when a host grant
is admitted. `subtree`, direct and derived `File` values, read/write/append,
exists/is-dir/list, directory creation, build input/output roots, and executable
selection then operate relative to retained handles. Writes atomically refuse a
symlink leaf. Linux executes the opened executable through an inherited procfd;
Unix hosts without descriptor execution run a private mode-checked snapshot of
that opened file. macOS platform binaries may instead use a path only after its
opened identity and root-owned, non-writable ancestry are verified. Mutable
grant pathnames are never reopened for execution. Both the interpreter oracle
and compiled host carry these shared objects.

Deterministic regressions replace a parent with an escaping symlink after
authority creation. Fresh operations reject it, while previously minted
subdirectory and file capabilities continue to address their original opened
objects. The browser target compiles the no-host-filesystem stub and cannot mint
ambient filesystem authority.

## The web platform: CSP is the browser's Landlock

coven-web already ships the hardened suite — per-page
`content-security-policy` plus COOP/COEP/CORP, `document-isolation-policy`,
`permissions-policy`, HSTS — and strict cross-origin isolation is a standing
project requirement. This RFC formalizes the *derivation* half:

- **`connect-src` = the granted `Fetch` origins.** The page-level fence
  matches the capability allowlist, so even a compromised runtime or an
  XSS'd page cannot exfiltrate beyond the origins the program was granted —
  the same statement Landlock makes about a compromised native host.
- **Baseline `default-src 'none'`** for pure playground programs;
  `script-src` admits only the page's own bundles plus
  `'wasm-unsafe-eval'` (required to instantiate wasm); `img-src`/`media-src`
  and friends stay `'none'` unless a glamour app's asset surface grants
  them.
- **Glamour sink hardening**: the derived CSP plus (future work) Trusted
  Types for glamour's `html`/`attr` sinks — the systemic mitigation for
  attribute-injection classes (e.g. unchecked `href` schemes), replacing
  point fixes.
- **Embedded runnable cells** (the book playground) run inside sandboxed
  iframes with the same derived policy, so a hostile example is fenced by
  the platform even where the wasm sandbox is the thing under test.
- The browser menu's `[enforcement]` section advertises `csp = "derived"`,
  keeping the native and web fences symmetric in the menu vocabulary.

Server-side, coven-web's `csp_for` becomes a consumer of the same
derivation (its policy today is hand-maintained; the capability surface of
the served app is the input it should derive from).

## Prior art

- **Chrome/Chromium**: layered sandbox (seccomp-bpf + user namespaces +
  broker) — the canonical "inner sandbox bugs are contained by an outer
  kernel fence" architecture.
- **OpenBSD `pledge`/`unveil`**: promise classes over raw syscall lists
  (the maintainability lesson this RFC's seccomp design adopts), and
  path-allowlist confinement shaped exactly like `Dir` grants.
- **systemd unit hardening**: declarative config compiled to kernel
  restrictions — the "policy derived from a declaration" precedent.
- **cap-std / Capsicum**: dirfd-anchored capability filesystems; `confine`'s
  own comment cites this as the race-free model, and FreeBSD's `cap_enter`
  is the natural Capsicum provider shape.
- **Landlock's own design**: unprivileged, stackable, restriction-only —
  built for exactly this "application confines itself at startup" pattern.
- **Deno**: permission flags with **no** kernel backing — the differentiator
  this RFC removes for witchy.

## Rejected alternatives

- **A hand-audited full seccomp allowlist**: breaks on every
  wasmtime/std/toolchain upgrade; the promise-class design bounds the
  maintenance to witchy's own class table with a CI canary.
- **Containers/user-namespaces as the mechanism**: heavyweight, often
  privileged, platform-fragmented; Landlock/seccomp are unprivileged,
  in-process, and dependency-free. (Container deployment remains available
  *around* witchy; it is not the language's fence.)
- **Making the kernel layer primary**: it is per-process and
  port/path-granular; it cannot express per-VM grants, address-level
  network policy, or Env/secret scoping. The host layer stays primary; the
  kernel layer bounds its failure.
- **Requiring platform enforcement everywhere**: would gate witchy on
  kernel versions and forbid platforms honestly lacking the machinery;
  best-effort + `--confine=required` covers both postures without parity
  risk.
- **eBPF LSM / custom policy engines**: deployment and privilege complexity
  for marginal expressiveness over Landlock+seccomp at this layer.

## Future work

- **pledge/unveil, Seatbelt, and Capsicum providers** — the derivation is
  shared; each backend is a bounded, owner-sized slice.
- **Trusted Types** for glamour sinks (named above; its own slice).
- **Windows** (AppContainer/restricted tokens) — recorded, unscheduled.
- **Per-VM kernel granularity** — if worker capability transfer ever lands
  (RFC-0102 future work), process-per-confined-VM becomes the mechanism to
  evaluate; out of scope here.
- **Landlock scoping of abstract sockets/signals** (ABI v6) as the ABI
  spreads.

## Implementation phases and evidence

1. **Implemented**: shared descriptor/handle-anchored `ConfinedDir` and
   `ConfinedFile` cover runtime and build reads/writes, append, navigation,
   metadata, listing, creation, direct grants, and executable selection.
   Evidence includes deterministic parent replacement, retained-authority,
   lexical escape, symlink-leaf write, interpreter traversal, compiled-exec
   parity, and browser-target compile regressions.
2. **Implemented**: `witchy-confinement` owns the normalized filesystem,
   network, Fetch-origin, and syscall-class policy, and
   `Capabilities::confinement_policy` is the only concrete-grant derivation.
   Empty grants remain distinct from absent authority and unexpressible
   transports are explicit. The Linux provider uses safe `landlock` APIs,
   best-effort or hard compatibility, separate filesystem/TCP rulesets, and
   Linux-gated host-bypass violation harnesses. Strict CLI sandbox and
   trusted-exe launchers arm that provider after grant resolution but before
   guest execution; reusable runtimes, development runs, parity, tests, servers,
   and in-process build orchestration explicitly leave irreversible process-wide
   policy disabled;
   evidence: a violation harness that deliberately bypasses the host layer
   in a test build and asserts the kernel denies (the fence catches a
   simulated host bug), plus the full differential suite green under the
   armed fence (superset invariant).
3. **Implemented**: Linux x86_64/aarch64/riscv64 hosts install a
   thread-synchronized seccomp-BPF filter after Landlock. Central `fs-open`,
   `net`, `listen`, and `proc` promise classes deny their syscall families
   with `EPERM` when the corresponding concrete authority is absent; a fixed
   bypass set denies `io_uring`, handle-based opens, mount/namespace mutation,
   BPF, and process-introspection mechanisms regardless of grants. The
   provider uses rust-vmm's safe `seccompiler` API, reports its own layer, and
   participates in required-mode completeness. Evidence: warning-denied Linux
   production and test-code cross-checks, class-set regressions, child probes
   proving `execve` and socket denial, and the suite-green strict launcher as
   the CI canary.
4. **Build-step confinement**: the build driver arms the fence around each
   `build.witchy` child from its `[build.grants]`; evidence: a build step
   attempting undeclared fs/net/exec is kernel-denied.
5. **Derived CSP**: playground + glamour hosts emit policy from the
   capability surface (`connect-src` = Fetch origins); coven-web's `csp_for`
   consumes the derivation; evidence: web test asserting the emitted policy
   matches the granted surface, and a probe page proving exfiltration to an
   ungranted origin is blocked by the browser.
6. **In progress**: `--confine=required` is implemented for strict source and
   precompiled sandbox launches, and the trusted-exe manifest's
   `confine = "required"` choice is authenticated inside the versioned binding
   plan. Unsupported, partial, or unexpressible enforcement fails before
   `main`; best-effort remains the default. The menu `[enforcement]` sections
   land with the remaining seccomp and CSP providers so they do not advertise
   unfinished mechanisms.

Each phase lands independently through the serialized gate; phases 1-3
touch the runtime TCB and take adversarial review.

## Compatibility

No program-observable change when the runtime is correct: the fence only
denies operations the host layer already denies (superset invariant), so
twin-backend parity and every existing test are unaffected by construction.
`Exec` child inheritance is the one deliberate behavior change (children
lose ambient authority); it ships with the declared child-needs vocabulary
and is called out in release notes as a breaking hardening. Platforms
without the machinery run exactly as today, minus the fence, and say so.

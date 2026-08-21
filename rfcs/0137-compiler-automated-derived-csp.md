---
rfc: 0137
title: "Compiler-Automated Derived CSP: Mathematical Confinement from Network Capabilities"
status: implemented
created: 2026-08-21
superseded-by:
tracking:
predecessors:
  - "[0013](0013-capability-grant-documents.md) (capability grant documents)"
  - "[0015](0015-secure-web-by-construction.md) (secure web applications by construction)"
  - "[0102](0102-portable-roots-and-the-fetch-capability.md) (portable roots and the Fetch capability)"
  - "[0103](0103-derived-platform-confinement.md) (derived platform confinement)"
  - "[0107](0107-glamour-next-generation-web-framework.md) (Glamour 1.0 web framework)"
  - "[0136](0136-impregnable-and-ergonomic-glamour.md) (impregnable and ergonomic Glamour)"
---

# RFC-0137: Compiler-Automated Derived CSP

## Summary

In modern web security, Content Security Policy (CSP) is the primary browser-enforced defense-in-depth mechanism. However, across the industry, CSPs are almost universally authored by hand, start overly permissive (`connect-src *; unsafe-inline`), and suffer constant policy drift.

In Witchy, **network authority is statically known, unforgeable, and declared via capabilities** (`Fetch`, `UiFetch`, `Net`). The compiler already knows the exact network boundaries of every component before execution begins.

This RFC introduces **Compiler-Automated Derived CSP**: the build toolchain and server runtime automatically derive, synthesize, and embed the minimal, exact CSP into static HTML artifacts, deployment manifests, server routing layers, and iframe compartments.

---

## 1. Motivation: Closing the Platform Confinement Loop

[RFC-0103](0103-derived-platform-confinement.md) established that platform confinement policies (Linux Landlock, seccomp, CSP) should be derived mechanically from Witchy capability grants. 

While native Landlock/seccomp enforcement operates automatically at binary launch, web-tier CSP enforcement historically required developers or server harnesses to configure headers.

By making CSP derivation an integrated compiler pass:
1. **Zero Configuration**: Developers never write a CSP string by hand.
2. **Zero Drift**: Adding, narrowing, or removing a `Fetch` capability immediately updates the compiled CSP policy.
3. **Outer Fence for Zero-Days**: Even if an attacker finds a theoretical JIT zero-day or memory-corruption escape within the WebAssembly runtime, the browser's own network kernel enforces `connect-src`, preventing data exfiltration to unauthorized origins.

---

## 2. Derivation Rules

The compiler evaluates the reachable capability AST and artifact manifest to synthesize the exact directive set:

| Capability / Toolchain Fact | Derived CSP Directive |
| :--- | :--- |
| **Empty Capability Footprint** (pure UI) | `connect-src 'none';` |
| `UiFetch(prefix: "/api/*")` | `connect-src 'self';` |
| `Fetch(origins: ["https://api.example.com"])` | `connect-src https://api.example.com;` |
| **Compiler Wasm/JS Bundles** | `script-src 'self' 'wasm-unsafe-eval' 'sha384-[digest]';` |
| **Compiler Scoped Styles** | `style-src 'self' 'sha384-[digest]';` |
| **No Compartments** | `frame-src 'none';` |
| `glamour.compartment(id: "chart")` | `frame-src /compartments/chart/;` |
| **Static Images** | `img-src 'self' data:;` |
| **Glamour DOM Shell** | `require-trusted-types-for 'script'; trusted-types glamour;` |
| **Default Baseline** | `default-src 'none'; base-uri 'none'; form-action 'self';` |

---

## 3. Delivery Channels

The derived CSP is emitted across three primary channels:

### 3.1 Static HTML `<meta>` Injection (Zero-Config Jamstack)
When compiling static sites (`witchy build --web` or `witchy compile --island`), the compiler embeds the derived CSP directly into the generated `index.html`:

```html
<!doctype html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta http-equiv="Content-Security-Policy" content="
        default-src 'none';
        connect-src 'self' https://api.coven.dev;
        script-src 'self' 'wasm-unsafe-eval' 'sha384-k8vN...';
        style-src 'self' 'sha384-j91a...';
        img-src 'self' data:;
        frame-src 'none';
        base-uri 'none';
        form-action 'self';
    ">
    <title>Witchy App</title>
</head>
<body>...</body>
</html>
```

### 3.2 Automated Server Header Generation
For server-rendered applications (`glamour-server`, `coven-web`), the compiler generates:
1. **`_headers` / Server Configs**: Pre-computed headers for Cloudflare Pages, Netlify, and Vercel.
2. **Server Middleware**: Witchy HTTP routers automatically attach the exact route-level CSP to HTTP responses:

```witchy
// The compiler binds this route's exact capability footprint to its response header
pub fn serve(assets: AssetStore, req: Request) -> Response:
    glamour_server.render_with_derived_csp(assets, req, app_view)
```

### 3.3 Dynamic Compartment Sub-CSPs
When the host mounts an untrusted third-party compartment (`mountCompartment`), the outer host enforces a sub-CSP directly on the iframe:

```html
<iframe 
    sandbox="allow-scripts" 
    csp="default-src 'none'; connect-src 'none'; script-src 'self'; style-src 'self';"
    src="/compartments/d3-chart/">
</iframe>
```

---

## 4. Security Invariants

1. **Strict Monotonicity**: A derived CSP is always a strict upper bound of the declared capability grants. An ungranted origin is never present in `connect-src`.
2. **No Permissive Escape Hatches**: The compiler will never emit `'unsafe-inline'` or `'unsafe-eval'` for scripts.
3. **Hash-Pinned Integrity**: All inline styles and entrypoint scripts must have their SHA-384 digests calculated at build time and pinned in the CSP.
4. **Trusted Types Backstop**: `require-trusted-types-for 'script'` is mandatory whenever DOM generation is active.

---

## Acceptance Criteria

1. `witchy build --web` emits valid, zero-warning CSP meta tags in all generated HTML entry points.
2. Capability analyzer (`witchy caps`) includes a `--csp` flag to preview the derived CSP string.
3. Automated integration test verifying that a simulated XSS/sandbox escape attempting an ungranted `fetch()` is aborted by the browser's CSP layer.

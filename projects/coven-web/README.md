# Coven Web

Coven Web is the browser-facing proof product for Witchy's package-trust story:
it presents Coven registry data through a Glamour application so humans can see
source, provenance, release state, and capability footprints in one place.

## Status

- **Status:** prototype/demo, not production-ready.
- **Server:** `src/coven_web.witchy`, a Witchy `std/server` application.
- **Frontend:** a capability-pure Glamour rune built into the web bundle.
- **Trusted shell:** small browser host code that owns browser authority and
  interprets the rune's inert `VNode` and `Cmd` data.
- **Security model:** summarized in [`SECURITY.md`](SECURITY.md); detailed
  historical plan in [`PLAN.md`](PLAN.md).

## What it demonstrates

- A same-origin web frontend for a Coven registry.
- Package catalog, version, source, trust, and capability-footprint views.
- Browser authority kept at the host-shell edge instead of inside the Witchy UI
  rune.
- Hardened browser assumptions such as strict CSP, Trusted Types, and
  compartment boundaries for foreign code.

## Run locally

Build the web bundle, then launch the local Coven registry plus Coven Web:

```sh
projects/coven-web/web/build.sh
projects/coven-web/dev.sh up --no-browser
```

`dev.sh` starts Coven and Coven Web on loopback defaults, seeds sample package
data unless disabled, and writes logs/PIDs under its store directory. Use
`projects/coven-web/dev.sh down` to stop the background processes.

For a self-contained verification run from the repository root:

```sh
python3 projects/coven-web/verify.py
```

That script spawns throwaway Coven and Coven Web servers, seeds a rune, and
checks the security headers, core proxy behavior, and authenticated write paths.
Treat any reported `FAIL` lines as prototype gaps to fix before describing Coven
Web as production-ready.

## Out of scope for this prototype note

- Rewriting the historical `PLAN.md` workstream log.
- Changing Coven's registry protocol.
- Claiming production readiness before the build path, CSP/authority boundaries,
  deterministic demo data, and browser verification are routine release gates.

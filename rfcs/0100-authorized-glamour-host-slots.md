---
rfc: 0100
title: Authority tokens for privileged Glamour host slots
status: deferred
created: 2026-07-19
superseded-by:
tracking: "split from RFC-0066; revive before any Glamour slot renderer receives secrets, network, filesystem, execution, or other host authority rather than presentation-only data"
---

# RFC-0100: Authority tokens for privileged Glamour host slots

## Summary

Require an unforgeable, renderer-scoped authority token before a Glamour rune
can invoke a host-slot renderer that carries authority beyond presentation.

The change is deferred while every committed slot renderer is
presentation-only: the host explicitly supplies a renderer map, renderers
receive only a document plus rune-provided data, and unknown kinds fall back to
an inert code block. A privileged renderer must not ship under that convention.

## Motivation

`glamour.slot(kind, data)` lets rune-controlled text select a renderer from the
host's configured map. That is safe for the current code/static/runnable-cell
presentation renderers, but the string becomes an authority selector if a
future renderer can read secrets, access a port, fetch, execute, or otherwise
exercise host authority. A naming convention is not an authorization check.

## Design constraints

- The host mints an opaque grant for an exact renderer kind and compartment.
- A privileged slot node carries that grant, not merely the renderer's string
  name; the host checks the grant before dispatch.
- Grants are unforgeable in both interpreter and compiled-Wasm representations,
  do not serialize through ordinary rune data, and cannot be widened by string
  composition or renderer aliases.
- Presentation-only slots remain possible without acquiring ambient host
  authority, but their renderer contract is mechanically incapable of effects.
- Tests prove an authorized dispatch, wrong-kind denial, cross-compartment
  denial, forged-data denial, and identical interpreter/compiled behavior.

## Revisit condition

Revive before registering the first slot renderer whose closure or returned
widget can exercise Secret, Net, Dir, Exec, ports, credential custody, or an
equivalent host effect. The privileged renderer and this authorization boundary
must land in the same merge-queue dependency stack.

## Alternatives

- **Trust the renderer map:** adequate only while the registered functions are
  effect-free presentation helpers.
- **Ban host slots:** removes a useful framework escape hatch and runnable-cell
  integration.
- **Treat every slot as privileged immediately:** safe but needlessly expands
  the current presentation ABI before a privileged consumer exists.

## Drawbacks

The future node and host ABI gain an opaque token and lifecycle rules. Deferral
keeps that complexity out of the presentation-only path without allowing a
privileged renderer to appear first.

## Prior art

Witchy's sealed capabilities and grant-bound host imports; RFC-0041 owns the
current host-slot rendering mechanism.

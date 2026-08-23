---
rfc: 0148
title: "Glamour: Boilerplate reduction via simple_program"
status: proposed
created: 2026-08-23
superseded-by:
tracking: "Drafting a lighter MVU trait for simple components"
---

# RFC-0148: Glamour simple_program API

## Summary

This RFC proposes the addition of `glamour.simple_program(initial, update, view)`, a lightweight wrapper around `glamour.program` that eliminates the need to explicitly write `authorize`, `start`, and `subscriptions` boilerplate for pure UI components that perform no side-effects and require no capabilities.

## Motivation

Witchy's Glamour framework enforces the Model-View-Update (MVU) pattern. While this is fantastic for large applications with complex side-effects, it creates a steep learning curve and significant boilerplate for simple interactive components (like a counter or a pure toggle). Developers are forced to write:
```witchy
fn authorize(_root: UiRoot) -> Nil: Nil
fn start(_auth: Nil, _model: Int) -> Cmd(Msg): NoCmd
fn subscriptions(_auth: Nil, _model: Int) -> Sub(Msg): NoSub
```
just to satisfy the type constraints of `glamour.program`.

## Proposed Design

Introduce a new helper `glamour.simple_program` which:
1. Hardcodes `authorize` to return `Nil`.
2. Hardcodes `start` to return `NoCmd`.
3. Hardcodes `subscriptions` to return `NoSub`.
4. Simplifies the `update` signature to `fn(Model, Msg) -> Model` (implicitly wrapping the result in `(Model, NoCmd)`).

This mirrors Elm's `Browser.sandbox` vs `Browser.element`, providing a smooth on-ramp for beginners and a cleaner DX for pure components.

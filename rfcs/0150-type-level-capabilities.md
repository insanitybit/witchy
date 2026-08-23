---
rfc: 0150
title: "Language: Type-Level Capability Requirements (Effect System)"
status: proposed
created: 2026-08-23
superseded-by:
tracking: "Drafting language support for capability-aware type signatures"
---

# RFC-0150: Type-Level Capability Requirements

## Summary

This RFC proposes a language-level enhancement to Witchy's capability model: allowing composite types (like `Ui(Msg)`) to statically declare the capabilities they require to be rendered or executed, e.g., `Ui(Msg) requires [UiFetch, SecretInput]`. This effectively introduces a lightweight Effect System, enabling the compiler to automatically bubble up capability footprints through the type system without manual parameter plumbing.

## Motivation

Witchy currently ensures capability security by requiring explicit parameter passing (e.g., `fn view(model: Model, fetch: UiFetch) -> Ui(Msg)`). While this secures the *construction* of the view, the resulting `Ui(Msg)` object erases that context. A parent component receives the `Ui(Msg)` but cannot easily see what authority its children are secretly wielding, except via external tooling (`witchy caps`).

By making capabilities part of the type signature itself, developers get immediate, in-editor feedback about the security footprint of a UI component before they even try to render it.

## Proposed Design

* Introduce the `requires [Caps...]` syntax as a qualifier on types.
* A function returning `Ui(Msg) requires [UiFetch]` cannot be called unless the caller *also* possesses the `UiFetch` capability in its scope, or returns a type that also delegates that requirement upwards.
* This brings Witchy closer to languages with Algebraic Effects (like Koka or Roc), making security footprints a first-class citizen of the type system.

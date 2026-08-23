---
rfc: 0147
title: "Glamour: html tagged literal enhancements (events and whitespace)"
status: proposed
created: 2026-08-23
superseded-by:
tracking: "Drafting improvements to the html macro for better DX"
---

# RFC-0147: Glamour html macro enhancements

## Summary

The `html"..."` tagged literal currently provides a powerful, compile-time verified way to construct Glamour `VNode` trees. However, it lacks ergonomic support for inline event bindings and handles whitespace (such as leading newlines) in a way that frequently causes "multiple root nodes" compilation errors. This RFC proposes upgrading the macro to safely parse event bindings (e.g., `@click=${Msg}`) and intelligently trim insignificant whitespace, significantly reducing view boilerplate.

## Motivation

When building Glamour applications, developers often start with the `html"..."` tagged literal because it closely resembles standard markup. However, as interactivity is added, developers hit a wall:
1. They must fall back to the verbose `glamour.element("button", [glamour.on_event(...)])` API because the macro rejects `@click` as an unsafe attribute.
2. A single newline before the root `<div>` results in a compilation error (`more than one root element`) due to the parser yielding a text node.

By making the macro event-aware and whitespace-forgiving, we can provide a developer experience similar to JSX or Lit-HTML while maintaining Witchy's strict, capability-safe guarantees.

## Proposed Design

1. **Insignificant Whitespace Trimming**: 
   The `html` parser will drop purely whitespace text nodes that exist between tags or before the root tag, unless explicitly requested.
2. **Event Binding Syntax**: 
   The macro will recognize attributes beginning with `@` (e.g., `@click`, `@input`).
   When encountering `@click=${Expr}`, the macro will lower this into `glamour.on_event("id", "click", glamour.event_msg(Expr))`.

## Drawbacks

* Slightly increases the complexity of the `glamour.planned(...)` compilation macro.
* May require escaping literal `@` characters in attributes if they are meant to be plain strings (though rare).

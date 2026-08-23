---
rfc: 0149
title: "Glamour: Actor-based UI Component Concurrency"
status: proposed
created: 2026-08-23
superseded-by:
tracking: "Drafting channel-based component orchestration"
---

# RFC-0149: Actor-based UI Component Concurrency

## Summary

This RFC proposes shifting Glamour's composition model from a rigid, synchronous top-down MVU hierarchy to an asynchronous, actor-based model that leverages Witchy's built-in `async` tasks and `chan` (channels). This aims to solve the "fractal MVU" problem by allowing nested components to manage their own local state and communicate via message passing.

## Motivation

In traditional MVU, composing components requires manually routing every child's `Msg` up to the parent's `update` function and manually drilling the child's `Model` back down. This scales poorly in large applications.

Witchy inherently excels at concurrent, cooperative task management. If UI components could be spawned as tasks, they could own their local state natively without requiring the parent to store it, drastically flattening the application structure.

## Proposed Design

* Introduce a `glamour.mount_task(component_fn, initial_state, channel_in, channel_out)` abstraction.
* Components are written as `async fn` server loops (`chan.serve`) that consume DOM events from `channel_in`, update their local state, and emit a new `VNode` tree to the framework.
* Parents and children communicate purely by sending explicit messages across typed Witchy channels, mirroring the Actor Model (similar to Erlang/Elixir LiveView or Cycle.js).

## Drawbacks

* Requires significant re-architecting of the Glamour diffing and patching engine to accept asynchronous, fine-grained `VNode` updates rather than a single top-level re-render.
* May complicate the current deterministic snapshot-testing guarantees.

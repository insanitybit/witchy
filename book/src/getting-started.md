# Getting Started

This chapter gets witchy onto your machine, runs your first program, and
introduces the handful of commands you'll use constantly. If you'd rather read
first and run later, you can — but witchy is small enough that poking at it is
the fastest way to learn it, and the browser playground means you don't even
need to install anything to start.

The three sections:

- [**Installation**](getting-started-installation.md) — building the `witchy`
  binary (and the zero-install playground).
- [**Hello, witchy**](getting-started-hello.md) — your first program, and what
  every piece of it means.
- [**The Toolbox**](getting-started-toolbox.md) — `run`, `check`, `parity`,
  `caps`, `sandbox`, `fmt`, `test`, and friends.

## Why capabilities matter (the one idea to hold onto)

witchy has one idea that shapes everything else, and it's worth meeting on
page one rather than chapter eight: **authority is a value you receive, never
something you can summon.** Your first program's `main` takes a `Console`
parameter, and *that is why* it's allowed to print — a function that isn't
handed a `Console` cannot print, a function without a `Dir` cannot touch the
filesystem, and there is no `import` or global that grants those powers behind
your back. You can read any witchy function's signature and know the complete
set of effects it can have.

That is the thing witchy does that mainstream languages don't, and the tour
chapters that follow (values, functions, data, errors) will feel like an
ordinary, pleasant small language until you reach
[Capabilities](capabilities.md), where the idea pays off — sandboxing,
supply-chain safety, and "provably no effects" all fall out of it. Keep the
one idea in mind as you go: **the signature is the whole story.**

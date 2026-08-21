# Getting Started

You don't have to install anything to read this book. The browser playground
runs every example here, compiled by the same compiler you'd install locally -
that compiler targets WebAssembly, so it runs in the page.

If you'd rather work locally, this chapter installs witchy, runs a first
program, and lists the core commands.

Sections:

- [**Installation**](getting-started-installation.md) - building the `witchy`
  binary (and the zero-install playground).
- [**Hello, witchy**](getting-started-hello.md) - your first program, and what
  every piece of it means.
- [**The Toolbox**](getting-started-toolbox.md) - `run`, `check`, `parity`,
  `caps`, `sandbox`, `fmt`, `test`, and friends.

## Capabilities

A function can exercise only authority it receives through typed values. `main`
takes a `Console` - that's why it can print directly. A function without a
`Console` possesses no direct printing authority, though an ordinary callback
may delegate a narrower printing operation. There's no import or global that
grants host powers.

```witchy
import show

fn main(c: Console):
    show.say(c, "hello")  // c is why this works
```

The signature is the complete set of effects. The
[Capabilities](capabilities.md) chapter covers sandboxing, supply-chain safety,
and effect-free guarantees that follow from this.

# Getting Started

This chapter installs witchy, runs a first program, and lists the core commands.
The browser playground can run the examples without a local installation.

Sections:

- [**Installation**](getting-started-installation.md) — building the `witchy`
  binary (and the zero-install playground).
- [**Hello, witchy**](getting-started-hello.md) — your first program, and what
  every piece of it means.
- [**The Toolbox**](getting-started-toolbox.md) — `run`, `check`, `parity`,
  `caps`, `sandbox`, `fmt`, `test`, and friends.

## Capabilities

A function can only do what its signature says. `main` takes a `Console` — that's
why it can print. A function without a `Console` cannot print. A function without
a `Dir` cannot touch the filesystem. There is no import or global that grants
these powers.

```witchy
import show

fn main(c: Console):
    show.say(c, "hello")  // c is why this works
```

The signature is the complete set of effects. The
[Capabilities](capabilities.md) chapter covers sandboxing, supply-chain safety,
and effect-free guarantees that follow from this.

# Appendix: The Ecosystem

The applications under [`projects/`](https://github.com/insanitybit/witchy/tree/master/projects)
use the language and toolchain to share code, build user interfaces, and run a
registry. [`projects/README.md`](https://github.com/insanitybit/witchy/blob/master/projects/README.md)
maps the repository.

## The pieces

- **coven** - the package **registry**. It stores signed records, recomputes
  each rune's capability footprint from source (never trusting declared
  metadata), and *blocks any publish that widens authority*. Publishing is
  two-phase: stage, then a human-2FA promote. The promotion gate enforces the
  rule that a dependency can't quietly gain authority. (Chapter:
  [Sharing Code: Runes and the Registry](packages.md).)

- **pm** - the package-manager **client** ("cargo for witchy"): resolve, fetch,
  verify, add, build, run. It talks to coven over a small HTTP contract.

- **glamour** - a capability-pure **frontend framework** (Model-View-Update).
  Your app is a pure function from state to a `VNode` tree, and it emits effects
  as inert `Cmd` *data*; a capability-holding host interprets them. The app
  itself holds no DOM, network, or storage authority - so a witchy UI has the
  same deny-by-default footprint story as any other witchy program.

- **coven-web** - the **web console** for coven: a pure-witchy server plus a
  thin host shell that holds the browser-side authority a pure-compute guest
  can't (network, session, credentials). It serves a glamour app same-origin
  with strict cross-origin isolation.

- **docs** - this book, rendered as a glamour app.

## Shared rule

All of these components treat **authority as a value you can compute, diff, and
gate**. The language records authority in capability types, coven gates package
footprints, and glamour represents browser effects as data for an authorized
host. See [Capabilities](capabilities.md) for the language rule.

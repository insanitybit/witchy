# Appendix: The Ecosystem

The book teaches the *language*. But witchy is also the foundation of a small
ecosystem of applications written **in** witchy — the pieces that let you share
code, build user interfaces, and run a registry. They live under
[`projects/`](https://github.com/insanitybit/witchy/tree/master/projects) in the
repository (`projects/README.md` is the map); this appendix is the one-paragraph
orientation.

## The pieces

- **coven** — the package **registry**. It stores signed records, recomputes
  each package's capability footprint from source (never trusting declared
  metadata), and *blocks any publish that widens authority*. Publishing is
  two-phase: stage, then a human-2FA promote. This is where "a dependency can't
  quietly gain new powers" is enforced. (Chapter:
  [Sharing Code: Runes and the Registry](packages.md).)

- **pm** — the package-manager **client** ("cargo for witchy"): resolve, fetch,
  verify, add, build, run. It talks to coven over a small HTTP contract.

- **glamour** — a capability-pure **frontend framework** (Model-View-Update).
  Your app is a pure function from state to a `VNode` tree, and it emits effects
  as inert `Cmd` *data*; a capability-holding host interprets them. The app
  itself holds no DOM, network, or storage authority — so a witchy UI has the
  same deny-by-default footprint story as any other witchy program.

- **coven-web** — the **web console** for coven: a pure-witchy server plus a
  thin host shell that holds the browser-side authority a pure-compute guest
  can't (network, session, credentials). It serves a glamour app same-origin
  with strict cross-origin isolation.

- **docs** — this book, rendered as a glamour app.

## Why it hangs together

One idea unifies them: **authority is a value you can compute, diff, and gate.**
The language makes authority explicit (the capabilities you learned about);
coven turns that into a supply-chain gate (footprints that can only shrink); and
glamour extends the same discipline to the browser (UI effects are data a host
must be authorized to run). The chapter on
[Capabilities](capabilities.md) is the concept; the ecosystem is that concept
applied end to end.

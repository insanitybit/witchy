# Witchy

Witchy is a capability-secure programming language with an interpreter and a
compiled WebAssembly backend. Programs declare the authority they need, and
the runtime enforces that boundary.

This repository is the public source tree for the compiler, standard library,
examples, and test suite. The supported surface is the behavior exercised by
the checked-in tests and examples.

## Build and test

Witchy requires Rust 1.95 or newer.

```sh
cargo build --workspace
cargo test --workspace
```

Run the compiler from the debug build with `target/debug/witchy`. The standard
library sources are in `std/`; runnable examples are in `examples/`.

## Project boundaries

`projects/coven` is a standalone Witchy project. `projects/pm` contains the
source for the embedded package-manager command and is tested through
`witchy pm`; it is not an independently buildable project because the CLI
supplies its Coven protocol modules at embed time. See
[`projects/pm/README.md`](projects/pm/README.md).

## Security

See [SECURITY.md](SECURITY.md) for vulnerability reporting.

## License

Licensed under either of Apache License 2.0 or MIT, at your option.

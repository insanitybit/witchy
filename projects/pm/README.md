# Embedded package manager

`pm.witchy` is the source for Witchy's built-in package-manager command. The
native CLI embeds it and supplies the shared Coven protocol modules from
`projects/coven/src/` at compile time.

Use the supported command through the compiler binary:

```sh
cargo build
target/debug/witchy pm help
```

The `pm` source is intentionally not a standalone project. A direct
`witchy build projects/pm` invocation cannot resolve its embedded protocol
modules and is not part of the supported project-build set. The embedded path
is covered by the workspace's package-manager and Coven integration tests.

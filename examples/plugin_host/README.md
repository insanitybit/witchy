# plugin_host

The capability thesis, made concrete. A "plugin" is just a pure
`fn(Int) -> Int`: with no capability parameters, it is structurally unable to
read a file, open a socket, or print — there is no Console/Dir/Net in scope for
it to use. The host owns all authority and decides what each plugin sees;
authority is opt-in (by parameter or by capture), never ambient.

**Shows:** function values as plugins, closures capturing a capability
(`console`), higher-order functions, `for` loops.

## Run

```sh
witchy run                                          # from this directory
witchy examples/plugin_host/src/plugin_host.witchy  # or by file, from the repo root
```

# branded_caps

A branded capability: `ConfigDir` is a newtype around a `Dir` that can only be
obtained through `config_dir`, a checked smart constructor that confirms the
config subtree exists. `load` then statically demands the brand, not just any
`Dir`, so the handle provably came through that gate. The capability auditor sees
through the brand — `ConfigDir` still audits as `Dir`, so authority can't hide.

**Shows:** the `Dir` capability, newtype-wrapped capabilities, smart constructors,
`Option`, `if let`, and `match`.

## Run

```sh
witchy run                                              # from this directory
witchy examples/branded_caps/src/branded_caps.witchy    # or by file, from the repo root
witchy caps examples/branded_caps/src/branded_caps.witchy   # load: Dir (refined: ConfigDir)
```

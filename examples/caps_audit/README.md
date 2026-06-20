# caps_audit

A tiny capability auditor written entirely in witchy. It reads a witchy source
file with a read-only `Dir` capability, asks the compiler for the file's
capability footprint (`compiler.footprint`, returned as JSON), parses it with
`std/json`, and prints the total authority the file demands — a self-hosted slice
of `witchy caps`, proof the toolchain is usable from within witchy.

**Shows:** the `Dir[Read]` capability, the `compiler` reflection module, JSON
parsing (`std/json`), `Option`, `if let`, and `match`.

## Run

```sh
witchy run                                            # from this directory
witchy examples/caps_audit/src/caps_audit.witchy      # or by file, from the repo root
```

# caps_guard

The supply-chain gate, written in witchy. It reads two versions of a rune with a
read-only `Dir` capability, asks the compiler whether the new one *widens* the
old's capability footprint (`compiler.diff`), and prints an ALLOW / BLOCK verdict
— the block-on-widening decision that guards a dependency upgrade. It exits 2 on a
blocked widening, so it can be wired into CI. Together with caps_audit, this is
the package manager's heart, in witchy.

**Shows:** the `Dir[Read]` capability, the `compiler` reflection module, JSON
parsing (`std/json`), process exit codes, and `match`.

## Run

```sh
witchy run                                            # from this directory
witchy examples/caps_guard/src/caps_guard.witchy      # or by file, from the repo root
```

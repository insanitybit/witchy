# Vendored Witchy Tree-sitter grammar

This directory vendors the Tree-sitter Witchy grammar at upstream commit
`78e5774217203a4dfc8be98e4ddd2f374cdd5e89` from the local
`tree-sitter-witchy` repository. The generated parser and grammar metadata are
checked in so Zed can compile the grammar reproducibly without depending on an
unpublished checkout.

The upstream package declares the MIT license. This vendor is part of the
Witchy repository and is additionally covered by the repository's
`LICENSE-MIT` and `LICENSE-APACHE` terms where applicable. No `.git`,
`node_modules`, cache, or machine-built Wasm artifacts are included.

To regenerate and run the upstream consistency test with Tree-sitter CLI
0.25.10 (from `package-lock.json`):

```sh
npm ci
npx tree-sitter generate
node test/references.mjs
```

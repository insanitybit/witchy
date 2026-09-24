# Witchy Zed Extension

Zed editor extension providing language support for Witchy:
- Tree-sitter syntax highlighting (`languages/witchy/highlights.scm`)
- Auto-indent and bracket matching (`indents.scm`, `brackets.scm`)
- Language Server Protocol integration with `witchy lsp` (`extension.toml`, `src/lib.rs`)

## Installation (Dev Extension)

1. Open Zed.
2. Open the Command Palette (`Cmd+Shift+P`).
3. Run **`zed: install dev extension`** (or open `zed: extensions` and click **Install Dev Extension**).
4. Select the directory:
   ```
   /Users/cobrien/workspace/witchy/editors/zed
   ```
5. Zed will read `extension.toml`, fetch the pinned Tree-sitter grammar, compile `witchy.wasm`, and register the `Witchy` language.

## Grammar Synchronization Workflow

The Tree-sitter grammar lives in `editors/tree-sitter-witchy` in this repository.
Zed checks out the pinned Witchy commit and loads the grammar from that path:

```toml
[grammars.witchy]
repository = "file:///Users/cobrien/workspace/witchy"
rev = "<commit-sha>"
path = "editors/tree-sitter-witchy"
```

When updating the grammar or highlight queries:

1. **Update grammar**: edit `editors/tree-sitter-witchy/grammar.js` in this repository.
2. **Regenerate and test**: run `npm test` in `editors/tree-sitter-witchy` and commit the grammar with its generated parser artifacts.
3. **Update pin in extension.toml**: set `rev` to a commit containing those artifacts.
4. **Sync queries**: keep `editors/zed/languages/witchy/highlights.scm` identical to `editors/tree-sitter-witchy/queries/highlights.scm`.
5. **Rebuild in Zed**: In Zed, open `zed: extensions` and click **Rebuild** on the Witchy extension (or re-run `zed: install dev extension`).

> **Warning: Stale Compiled Grammar Trap**
> In Zed, if `highlights.scm` references a node type or keyword token not present in the compiled `grammars/witchy.wasm`, Tree-sitter query compilation aborts with `Invalid node type "..."`.
> When query compilation fails, Zed aborts the entire registration of the `Witchy` language. This disables syntax highlighting **and** prevents `witchy lsp` from launching (because language matching fails). Always reinstall or rebuild the dev extension whenever `rev` or `highlights.scm` changes.

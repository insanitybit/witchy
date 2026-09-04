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

The Tree-sitter grammar lives in the local repository `/Users/cobrien/workspace/tree-sitter-witchy` and is pinned by commit SHA in `extension.toml`:

```toml
[grammars.witchy]
repository = "file:///Users/cobrien/workspace/tree-sitter-witchy"
rev = "<commit-sha>"
```

When updating the grammar or highlight queries:

1. **Update grammar**: edit `grammar.js` in `/Users/cobrien/workspace/tree-sitter-witchy`.
2. **Regenerate parser**: run `npx tree-sitter generate`.
3. **Verify corpus**: verify that standard library and projects parse cleanly:
   ```sh
   for f in /Users/cobrien/workspace/witchy/std/*.witchy; do
     npx tree-sitter parse "$f" > /dev/null || echo "Failed: $f"
   done
   ```
4. **Commit in tree-sitter-witchy**: `git commit -am "..."`.
5. **Update pin in extension.toml**: update `rev` to the new commit SHA.
6. **Sync queries**: ensure `editors/zed/languages/witchy/highlights.scm` matches `tree-sitter-witchy/queries/highlights.scm`.
7. **Rebuild in Zed**: In Zed, open `zed: extensions` and click **Rebuild** on the Witchy extension (or re-run `zed: install dev extension`).

> **Warning: Stale Compiled Grammar Trap**
> In Zed, if `highlights.scm` references a node type or keyword token not present in the compiled `grammars/witchy.wasm`, Tree-sitter query compilation aborts with `Invalid node type "..."`.
> When query compilation fails, Zed aborts the entire registration of the `Witchy` language. This disables syntax highlighting **and** prevents `witchy lsp` from launching (because language matching fails). Always reinstall or rebuild the dev extension whenever `rev` or `highlights.scm` changes.

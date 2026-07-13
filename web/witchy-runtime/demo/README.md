# witchy glamour — real-browser demo

A self-contained, runnable proof that **footprint-empty witchy runes render and
update in an actual browser**. It exercises the full glamour frontend stack
(RFCs 0006 / 0007 / 0008):

- **RFC-0006** — the `html"…"` compile-time tagged literal that the counter's
  `view` is written with.
- **RFC-0007** — `witchy-runtime.mjs`, the deny-all *pure-compute* WASM host.
  These runes import only pure functions, so they instantiate; a rune that
  touched Net/Dir/Clock would fail with a `LinkError` (deny-by-omission).
- **RFC-0008** — `glamour-dom.mjs`, the capability-holding DOM shell that diffs
  a rune's VNode tree into the real DOM and marshals click events back as `Msg`
  values.

## What it shows

1. **Counter** — an MVU app (`Model = Int`, `Msg = Inc | Dec`) mounted via
   `mount(counterBytes, el)`. Clicking `+` / `-` dispatches a `Msg` into the
   rune (`export_step`), folds the model, and the differ patches the DOM. The
   buttons are tagged `data-action="inc"` / `data-action="dec"` and the count
   `<span>` is tagged `data-role="count"` so a test can find them.
2. **Highlighter** — a pure syntax highlighter. We call its `export_render({src})`
   through the shim's `callString` and render the returned VNode JSON into
   `#highlight` with `createElement` / `textContent` ONLY. The source includes a
   comment, a string, and a literal `<script>`; the `<script>` appears as inert,
   escaped **text** — never a real element — because every token flows through a
   glamour `text(...)` node (no `innerHTML` anywhere).

## Build

From this `demo/` directory:

```sh
./build.sh
```

This compiles the four runes (counter, highlighter, runecard, covenbrowser),
staging `glamour.witchy` as a sibling so `import glamour` resolves, and writes
each `*.wasm` here. The script uses the release binary if available, then falls
back to debug or PATH.

> Requires a witchy binary (`cargo build --release` or `cargo build`). The runes
> are footprint-empty, so they instantiate under the deny-all shim.

## Serve & open

The page imports `../witchy-runtime.mjs` and `../glamour-dom.mjs` with relative
paths, so it must be served from the **`web/witchy-runtime/`** parent directory:

```sh
cd ..                 # into web/witchy-runtime/
python3 -m http.server 8099
```

Then open <http://localhost:8099/demo/>.

You should see the counter (click `+`/`-` to change the number) and the
highlighted snippet with colored keyword / string / comment spans, with the
`<script>` shown as literal text. A status line at the bottom summarizes the
span counts and confirms `real <script> elements=0`.

## Files

- `build.sh` — compiles all four runes to `*.wasm`.
- `index.html` — the page (`#counter` + `#highlight` mount points, styling).
- `demo.mjs` — fetches the wasm, mounts the counter, renders the highlighter.
- `*.wasm` — build outputs (counter, highlighter, runecard, covenbrowser; created by `build.sh`).

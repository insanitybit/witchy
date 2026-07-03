// The witchy-book page boot (RFC-0041): mount the compiled docs app into the page, wiring the
// browser's real fetch / history / DOM and the runnable-cell slot renderer. The witchy rune is
// pure (capability-free); THIS glue holds the authority — exactly the shape the tested docs
// driver (`web/witchy-runtime/glamour-docs.test.mjs`) uses, only with real globals instead of
// injected fakes. So there is no server: the page fetches static content and runs snippets
// client-side.
import { mount } from "./glamour-dom.mjs";
import { runnableSlot } from "./witchy-runnable.js";
import { assetUrl, contentUrl } from "./docs-asset-url.js";
import { highlightWitchy } from "./witchy-highlight.js";

// Every bundle asset resolves against THIS module's URL — the bundle root — never the current
// route. A chapter routes to `/p/<slug>`, so a page-relative `./content/...` / `./witchy.wasm`
// would resolve to `/p/content/...` (a 404); `import.meta.url` is immune to the route and still
// honours a deploy subpath (GitHub Pages `/<repo>/`, where this module is `/<repo>/docs-boot.js`).
const here = import.meta.url;

// The browser compiler (`witchy.wasm`) is fetched + instantiated ONCE, lazily, on the first Run
// on the page, then shared by every cell (the browser HTTP-caches the 4 MB module).
const loadCompiler = (() => {
  let p = null;
  return () =>
    (p ||= fetch(assetUrl("witchy.wasm", here))
      .then((r) => r.arrayBuffer())
      .then((b) => WebAssembly.instantiate(b, {}))
      .then(({ instance }) => instance.exports));
})();

// The rune builds absolute `/content/...` fetch URLs; resolve them against the bundle root so a
// chapter route can never make the fetch route-relative.
const contentFetch = (url) => fetch(contentUrl(url, here));

const wasm = await fetch(assetUrl("docs.wasm", here)).then((r) => r.arrayBuffer());
await mount(wasm, document.getElementById("app"), {
  initialModel: { route: location.pathname, summary: "", content: "" },
  fetch: contentFetch,
  routeTag: "Route",
  location,
  history,
  // (RFC-0040) the host mints the app's `UiRoot`; the policy value is just a label.
  instantiateOpts: { userCaps: [["witchy-book"]] },
  // (RFC-0041) each `witchy` fence becomes an editable, runnable, SYNTAX-HIGHLIGHTED cell in
  // a non-diffed slot. `highlightWitchy` escapes its input (XSS-safe), so it can paint the
  // overlay via innerHTML.
  slots: { "witchy-runnable": runnableSlot({ loadCompiler, highlight: highlightWitchy }) },
});

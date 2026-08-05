// The witchy-book page boot (RFC-0041): mount the compiled docs app into the page, wiring the
// browser's real fetch / history / DOM and the runnable-cell slot renderer. The witchy rune is
// pure (capability-free); THIS glue holds the authority — exactly the shape the tested docs
// driver (`web/witchy-runtime/glamour-docs.test.mjs`) uses, only with real globals instead of
// injected fakes. So there is no server: the page fetches static content and runs snippets
// client-side.
import { mount } from "./glamour-dom.mjs";
import { runnableSlot, staticSlot } from "./witchy-runnable.js";
import { createSandboxedProgramRunner } from "./witchy-cell-sandbox.js";
import { assetUrl, contentUrl } from "./docs-asset-url.js";
import { fetchWasm } from "./wasm-fetch.js";
import { highlightWitchy, highlightShell, highlightToml } from "./witchy-highlight.js";
import { DOCS_SANDBOX_RUN_OPTIONS } from "./docs-run-options.js";

// Every bundle asset resolves against THIS module's URL — the bundle root — never the current
// route. A chapter routes to `/p/<slug>`, so a page-relative `./content/...` / `./witchy.wasm`
// would resolve to `/p/content/...` (a 404); `import.meta.url` is immune to the route and still
// honours a deploy subpath (GitHub Pages `/<repo>/`, where this module is `/<repo>/docs-boot.js`).
const here = import.meta.url;

// The browser compiler (`witchy.wasm`) is fetched + instantiated ONCE, lazily, on the first Run
// on the page, then shared by every cell (the browser HTTP-caches the 4 MB module). `fetchWasm`
// fails LOUDLY when the server answers with an HTML 404/index page instead of wasm — the mark
// of a bundle built WITHOUT the browser compiler (`--allow-missing-compiler` / `just book`).
const COMPILER_HINT =
  "This bundle was built without the browser compiler; rebuild the complete bundle with `just book-serve` (or ./scripts/build-docs.sh dist).";
const loadCompiler = (() => {
  let p = null;
  return () =>
    (p ||= fetchWasm(assetUrl("witchy.wasm", here), { hint: COMPILER_HINT })
      .then(async (bytes) => {
        const { module, instance } = await WebAssembly.instantiate(bytes, {});
        return { bytes, module, exports: instance.exports };
      }));
})();
const runProgram = createSandboxedProgramRunner({ document, loadCompiler });

// The rune builds absolute `/content/...` fetch URLs; resolve them against the bundle root so a
// chapter route can never make the fetch route-relative.
const contentFetch = (url) => fetch(contentUrl(url, here));

// (RFC-0041) A `glamour-app` fence names a bundled INTERACTIVE demo. Mount it LIVE with the
// book's OWN glamour-dom runtime into a contained sub-tree. NETWORK IS DENIED — no `fetch` is
// passed, so any `UiFetch`/http Cmd the app emits simply isn't performed; a timer IS provided
// so an interactive/animated app works. This trusted bundled app remains in the
// main frame; unlike an arbitrary runnable cell, it is not sent through the
// opaque-frame program runner. Each app's initial model must match its own shape.
const DEMO_APPS = { counter: { initialModel: { n: 0 } } };
const glamourAppSlot = (doc, name) => {
  const host = doc.createElement("div");
  host.setAttribute("class", "glamour-app");
  const cfg = DEMO_APPS[name];
  if (!cfg) {
    host.textContent = `unknown demo app: ${name}`;
    return host;
  }
  (async () => {
    try {
      const bytes = await fetchWasm(assetUrl(name + ".wasm", here), {
        hint: "Rebuild the bundle with `just book-serve` (or ./scripts/build-docs.sh dist).",
      });
      await mount(bytes, host, {
        initialModel: cfg.initialModel,
        // NO `fetch` (network denied). A timer is safe and enables `after` Cmds.
        setTimeout: (fn, ms) => setTimeout(fn, ms),
      });
    } catch (e) {
      host.textContent = "demo failed to load: " + ((e && e.message) || e);
    }
  })();
  return host;
};

// HASH ROUTING so a refresh or a shared deep link works on ANY static host — `python3 -m
// http.server`, GitHub Pages, anything. The `#…` fragment is never sent to the server, so it
// always serves index.html and the app routes from the hash (no server SPA-fallback, no 404s).
// The app still emits normal `/p/<slug>` nav; we adapt it to the hash through glamour-dom's
// injectable history/location/popstate hooks, so glamour-dom itself is unchanged.
const hashPath = () => {
  const h = window.location.hash;
  return h && h.length > 1 ? h.slice(1) : "/";
};
let navProgrammatic = false;
const historyShim = {
  pushState: (_state, _title, path) => {
    navProgrammatic = true;
    window.location.hash = path;
  },
};
const locationShim = { get pathname() { return hashPath(); } };
const onPopState = (fire) => {
  window.addEventListener("hashchange", () => {
    // glamour-dom already dispatched the route for OUR nav; only a user hash change
    // (Back/Forward, refresh, manual edit, a shared link) needs to re-fire.
    if (navProgrammatic) { navProgrammatic = false; return; }
    fire();
  });
};

const wasm = await fetchWasm(assetUrl("docs.wasm", here), {
  hint: "Rebuild the bundle with `just book-serve` (or ./scripts/build-docs.sh dist).",
});
await mount(wasm, document.getElementById("app"), {
  initialModel: { route: hashPath(), summary: "", content: "" },
  fetch: contentFetch,
  routeTag: "Route",
  location: locationShim,
  history: historyShim,
  onPopState,
  // (RFC-0040) the host mints the app's `UiRoot`; the policy value is just a label.
  instantiateOpts: { userCaps: [["witchy-book"]] },
  // (RFC-0041) each `witchy` fence becomes a SYNTAX-HIGHLIGHTED cell in a non-diffed slot:
  // a runnable one (editable + Run) where the snippet actually runs in the cell, or a
  // read-only one for a capability example that can't (so it never shows a Run error).
  // `highlightWitchy` escapes its input (XSS-safe), so it can paint via innerHTML.
  slots: {
    "witchy-runnable": runnableSlot({
      runProgram,
      highlight: highlightWitchy,
      runOptions: DOCS_SANDBOX_RUN_OPTIONS,
    }),
    "witchy-static": staticSlot({ highlight: highlightWitchy }),
    // Non-witchy code fences get read-only highlighting too (same token theme).
    "shell-static": staticSlot({ highlight: highlightShell }),
    "toml-static": staticSlot({ highlight: highlightToml }),
    // A `glamour-app` fence names a bundled INTERACTIVE demo; mount it live.
    "glamour-app": glamourAppSlot,
  },
});

// The witchy-book page boot (RFC-0041): mount the compiled docs app into the page, wiring the
// browser's real fetch / history / DOM and the runnable-cell slot renderer. The witchy rune is
// pure (capability-free); THIS glue holds the authority — exactly the shape the tested docs
// driver (`web/witchy-runtime/glamour-docs.test.mjs`) uses, only with real globals instead of
// injected fakes. So there is no server: the page fetches static content and runs snippets
// client-side.
import { mount } from "./glamour-dom.mjs";
import { runnableSlot } from "./witchy-runnable.js";

// The browser compiler (`witchy.wasm`) is fetched + instantiated ONCE, lazily, on the first Run
// on the page, then shared by every cell (the browser HTTP-caches the 4 MB module).
const loadCompiler = (() => {
  let p = null;
  return () =>
    (p ||= fetch("./witchy.wasm")
      .then((r) => r.arrayBuffer())
      .then((b) => WebAssembly.instantiate(b, {}))
      .then(({ instance }) => instance.exports));
})();

// The rune builds absolute `/content/...` fetch URLs; make them relative to the page so the
// bundle works under any deploy subpath (e.g. GitHub Pages `/<repo>/`).
const contentFetch = (url) => fetch(typeof url === "string" && url.startsWith("/") ? "." + url : url);

const wasm = await fetch("./docs.wasm").then((r) => r.arrayBuffer());
await mount(wasm, document.getElementById("app"), {
  initialModel: { route: location.pathname, summary: "", content: "" },
  fetch: contentFetch,
  routeTag: "Route",
  location,
  history,
  // (RFC-0040) the host mints the app's `UiRoot`; the policy value is just a label.
  instantiateOpts: { userCaps: [["witchy-book"]] },
  // (RFC-0041) each `witchy` fence becomes an editable, runnable cell in a non-diffed slot.
  slots: { "witchy-runnable": runnableSlot({ loadCompiler }) },
});

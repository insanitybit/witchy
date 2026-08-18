// Progressive host for runnable Witchy fences in the statically published book.
// The application HTML and Glamour islands are already complete; this module
// runs only on routes whose checked output contains a runnable host marker.
import { enhanceRunnableCells, enhanceStaticSlots } from "./witchy-runnable.js";
import { createSandboxedProgramRunner } from "./witchy-cell-sandbox.js";
import { assetUrl } from "./docs-asset-url.js";
import { fetchWasm } from "./wasm-fetch.js";
import { highlightWitchy, highlightShell, highlightToml } from "./witchy-highlight.js";
import { DOCS_SANDBOX_RUN_OPTIONS } from "./docs-run-options.js";

const here = import.meta.url;
const loadCompiler = (() => {
  let pending = null;
  return () =>
    (pending ||= fetchWasm(assetUrl("witchy.wasm", here), {
      hint: "This book bundle omits its browser compiler; rebuild it without --allow-missing-compiler.",
    }).then(async (bytes) => {
      const { module, instance } = await WebAssembly.instantiate(bytes, {});
      return { bytes, module, exports: instance.exports };
    }));
})();

const runProgram = createSandboxedProgramRunner({ document, loadCompiler });
if (document.querySelector("script[data-witchy-islands]")
    && !document.documentElement.hasAttribute("data-witchy-islands-ready")) {
  await new Promise((resolve, reject) => {
    document.addEventListener("witchy-islands-ready", resolve, { once: true });
    document.addEventListener(
      "witchy-islands-failed",
      () => reject(new Error("Glamour islands failed before runnable-cell adoption")),
      { once: true },
    );
  });
}
enhanceStaticSlots(document.body, {
  highlight: highlightWitchy,
  highlightShell,
  highlightToml,
});
enhanceRunnableCells(document.body, {
  runProgram,
  highlight: highlightWitchy,
  runOptions: DOCS_SANDBOX_RUN_OPTIONS,
});

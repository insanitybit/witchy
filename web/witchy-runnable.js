// witchy-runnable — turn the docs' `witchy` code blocks into RUNNABLE cells (RFC-0041 P2).
//
// After the glamour docs app renders a page, call `enhanceRunnableCells(root, opts)`: it
// finds every `pre > code.language-witchy` (the class `markdown.to_vnode` now emits) and adds
// a Run button + an output pane. On Run, it compiles+runs the block's source with the shared
// `witchy-host.js` engine against the lazily-loaded compiler (`web/witchy.wasm`) and shows the
// output. A reader's snippet compiles to a capability-DENIED, pure-compute wasm module
// (deny-by-omission — the browser grants no Dir/Net/…), so it is already contained; that is
// why a cell runs safely in the MAIN frame and needs no compartment (RFC-0041 §Phase 2).
//
// The enhancer is deliberately DOM-agnostic (createElement / getAttribute / childNodes /
// appendChild / replaceChild only — no innerHTML sink, no querySelector) so it runs under a
// real browser DOM AND a headless FakeElement DOM, and is idempotent so it is safe to call
// after every render.

import { runWitchy } from "./witchy-host.js";

// A `<code>` element in either DOM flavour (FakeElement uses `.el`, the browser `.tagName`).
function isCode(n) {
  const tag = n.el || (typeof n.tagName === "string" ? n.tagName.toLowerCase() : "");
  return tag === "code";
}
function classOf(n) {
  return (typeof n.getAttribute === "function" && n.getAttribute("class")) || "";
}

// Collect every `<pre> > <code class="…language-witchy…">` under `root`.
function findWitchyCells(root, acc = []) {
  const kids = root.childNodes || [];
  for (const child of kids) {
    if (isCode(child) && classOf(child).split(/\s+/).includes("language-witchy")) {
      const pre = child.parentNode;
      if (pre) acc.push({ pre, code: child });
    } else {
      findWitchyCells(child, acc);
    }
  }
  return acc;
}

/**
 * Enhance runnable `witchy` code blocks under `root`.
 *
 * @param {Node} root
 * @param {object} opts
 * @param {Document} [opts.document]     the DOM document (default the global `document`).
 * @param {function} opts.loadCompiler   async `() => wasmExports` — the instantiated
 *   `web/witchy.wasm` exports; called lazily on the first Run and cached. Injectable so a
 *   headless harness can supply the exports (the browser fetches + instantiates the module).
 * @returns {Array} the enhanced cells (`{ pre, code, runButton, output, run }`).
 */
export function enhanceRunnableCells(root, opts = {}) {
  const doc = opts.document || (typeof document !== "undefined" ? document : null);
  if (!doc) throw new Error("witchy-runnable: no `document` (pass opts.document)");
  if (typeof opts.loadCompiler !== "function") {
    throw new Error("witchy-runnable: opts.loadCompiler must be an async () => wasm exports");
  }
  // The compiler is fetched+instantiated once, lazily, on the first Run — and shared.
  let compilerPromise = null;
  const compiler = () => (compilerPromise ||= Promise.resolve().then(opts.loadCompiler));

  const cells = [];
  for (const { pre, code } of findWitchyCells(root)) {
    if (pre.__witchyEnhanced) continue; // idempotent — safe to re-run after each render
    pre.__witchyEnhanced = true;
    const source = code.textContent;

    const runButton = doc.createElement("button");
    runButton.setAttribute("class", "witchy-run");
    runButton.textContent = "Run";

    const output = doc.createElement("pre");
    output.setAttribute("class", "witchy-output");

    const run = async () => {
      runButton.textContent = "Running…";
      try {
        const wasm = await compiler();
        const { ok, text } = await runWitchy(wasm, source);
        output.textContent = text || (ok ? "(no output)" : "(empty error)");
        output.setAttribute("class", ok ? "witchy-output ok" : "witchy-output err");
      } catch (e) {
        output.textContent = "playground error: " + ((e && e.message) || e);
        output.setAttribute("class", "witchy-output err");
      }
      runButton.textContent = "Run";
    };
    if (typeof runButton.addEventListener === "function") {
      runButton.addEventListener("click", run);
    }

    // Wrap `<pre>` + Run + output in a cell, in place of the bare `<pre>`. Run is
    // HOST-managed (it never dispatches a glamour msg), so glamour never re-diffs the cell.
    const cell = doc.createElement("div");
    cell.setAttribute("class", "witchy-cell");
    const parent = pre.parentNode;
    if (parent) parent.replaceChild(cell, pre);
    cell.appendChild(pre);
    cell.appendChild(runButton);
    cell.appendChild(output);

    cells.push({ pre, code, runButton, output, run });
  }
  return cells;
}

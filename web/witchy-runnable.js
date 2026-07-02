// witchy-runnable — build RUNNABLE cells from `witchy` code blocks (RFC-0041 P2).
//
// A reader's snippet compiles to a capability-DENIED, pure-compute wasm module
// (deny-by-omission — the browser grants no `Dir`/`Net`/…), so it is already contained by
// wasm's memory isolation plus the capability model: it can only compute and return a string.
// That is why a cell runs safely in the MAIN frame with no compartment (RFC-0041 §Phase 2).
//
// Two entry points:
//   * `runnableSlot(opts)` — a glamour HOST-SLOT renderer: register it as
//     `mount(..., { slots: { "witchy-runnable": runnableSlot({ loadCompiler }) } })`. glamour
//     mounts it into a `Slot` node and NEVER diffs into it, so a page re-render can't clobber
//     the cell. This is what the docs app uses.
//   * `enhanceRunnableCells(root, opts)` — progressive enhancement for a STATIC page (the
//     standalone playground): find every `pre>code.language-witchy` and replace it with a cell.
//
// Both build the same cell via `buildRunnableCell`. DOM-agnostic (createElement / getAttribute
// / childNodes only — no innerHTML sink, no querySelector) so it runs under a real browser DOM
// AND a headless FakeElement DOM.

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
 * Build a runnable cell for `source`: a `<pre>` showing the code, a Run button, and an output
 * pane. On Run, compile+run `source` via `witchy-host.js` against the lazily-loaded compiler.
 *
 * @param {Document} doc
 * @param {string} source
 * @param {object} opts
 * @param {function} opts.loadCompiler  async `() => wasmExports` — the instantiated
 *   `web/witchy.wasm` exports; called lazily on the first Run and cached per cell.
 * @returns {{ element, codePre, runButton, output, run }}
 */
export function buildRunnableCell(doc, source, opts = {}) {
  if (typeof opts.loadCompiler !== "function") {
    throw new Error("witchy-runnable: opts.loadCompiler must be an async () => wasm exports");
  }
  let compilerPromise = null;
  const compiler = () => (compilerPromise ||= Promise.resolve().then(opts.loadCompiler));

  const element = doc.createElement("div");
  element.setAttribute("class", "witchy-cell");
  // The source is EDITABLE: a `<textarea>` seeded with the block. Because the docs app mounts
  // the cell in a non-diffed host Slot, an edit survives page re-renders. The textarea carries
  // no `language-witchy` class, so a standalone re-scan (`enhanceRunnableCells`) never
  // re-enhances an already-built cell.
  const editor = doc.createElement("textarea");
  editor.setAttribute("class", "witchy-editor");
  editor.setAttribute("spellcheck", "false");
  editor.value = source;

  const runButton = doc.createElement("button");
  runButton.setAttribute("class", "witchy-run");
  runButton.textContent = "Run";
  const output = doc.createElement("pre");
  output.setAttribute("class", "witchy-output");

  const run = async () => {
    // Run the CURRENT editor contents (the reader's edits), falling back to the seed source.
    const src = typeof editor.value === "string" ? editor.value : source;
    runButton.textContent = "Running…";
    try {
      const wasm = await compiler();
      const { ok, text } = await runWitchy(wasm, src);
      output.textContent = text || (ok ? "(no output)" : "(empty error)");
      output.setAttribute("class", ok ? "witchy-output ok" : "witchy-output err");
    } catch (e) {
      output.textContent = "playground error: " + ((e && e.message) || e);
      output.setAttribute("class", "witchy-output err");
    }
    runButton.textContent = "Run";
  };
  if (typeof runButton.addEventListener === "function") runButton.addEventListener("click", run);
  // ⌘/Ctrl-Enter runs, matching the standalone playground.
  if (typeof editor.addEventListener === "function") {
    editor.addEventListener("keydown", (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
        if (typeof e.preventDefault === "function") e.preventDefault();
        run();
      }
    });
  }

  element.appendChild(editor);
  element.appendChild(runButton);
  element.appendChild(output);
  return { element, editor, runButton, output, run };
}

/**
 * A glamour host-slot renderer (RFC-0041): mounts a runnable cell for the slot's source.
 * Register as `slots["witchy-runnable"]`; glamour hands it `(doc, source)` and never diffs the
 * returned node.
 */
export function runnableSlot(opts = {}) {
  return (doc, source) => buildRunnableCell(doc, source, opts).element;
}

/**
 * Standalone progressive enhancement (a static page / the playground): find every
 * `pre>code.language-witchy` under `root` and REPLACE it with a runnable cell. Idempotent (a
 * built cell's code carries no `language-witchy` class) and DOM-agnostic. The glamour docs app
 * uses `runnableSlot` instead, so glamour owns the non-diffed slot.
 */
export function enhanceRunnableCells(root, opts = {}) {
  const doc = opts.document || (typeof document !== "undefined" ? document : null);
  if (!doc) throw new Error("witchy-runnable: no `document` (pass opts.document)");
  const cells = [];
  for (const { pre, code } of findWitchyCells(root)) {
    const cell = buildRunnableCell(doc, code.textContent, opts);
    const parent = pre.parentNode;
    if (parent) parent.replaceChild(cell.element, pre);
    cells.push({ pre, code, ...cell });
  }
  return cells;
}

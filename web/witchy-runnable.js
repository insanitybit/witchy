// witchy-runnable — build RUNNABLE cells from `witchy` code blocks (RFC-0041 P2).
//
// A reader's snippet compiles to tree-shaken wasm whose imports are exactly the
// authority it reaches. The host starts deny-by-omission and may pass explicit
// `runOptions` providers for examples admitted by the browser menu. Production
// callers inject a sandbox runner: the docs runner compiles in the trusted page
// and executes the guest in a fresh opaque-origin iframe (RFC-0103).
//
// Two entry points:
//   * `runnableSlot(opts)` — a glamour HOST-SLOT renderer: register it as
//     `mount(..., { slots: { "witchy-runnable": runnableSlot({ runProgram }) } })`. glamour
//     mounts it into a `Slot` node and NEVER diffs into it, so a page re-render can't clobber
//     the cell. This is what the docs app uses.
//   * `enhanceRunnableCells(root, opts)` — progressive enhancement for a STATIC page (the
//     standalone playground): find every `pre>code.language-witchy` and replace it with a cell.
//
// Both build the same cell via `buildRunnableCell`. DOM-agnostic (createElement / getAttribute
// / childNodes only — no innerHTML sink, no querySelector) so it runs under a real browser DOM
// AND a headless FakeElement DOM.

function formatOptimizationStats(stats) {
  const entries = Object.entries(stats || {});
  if (entries.length === 0) return "";
  return ["compiled resource counters", ...entries.map(([name, value]) => `${name} ${value}`)].join("\n");
}

// A `<code>` element in either DOM flavour (FakeElement uses `.el`, the browser `.tagName`).
function isCode(n) {
  const tag = n.el || (typeof n.tagName === "string" ? n.tagName.toLowerCase() : "");
  return tag === "code";
}
function classOf(n) {
  return (typeof n.getAttribute === "function" && n.getAttribute("class")) || "";
}

const adoptedRunnableCells = new WeakSet();

// (RFC-0041) Optional syntax highlighting: overlay the editable textarea on a `<pre>`
// painted by `opts.highlight` (an XSS-safe `source -> HTML`, e.g. `highlightWitchy`). The
// textarea stays on top for input + selection; its text is transparent so the coloured pre
// shows through. Both share identical type metrics (CSS) so the caret aligns. Degrades to
// the bare textarea where `innerHTML` is unavailable (the headless FakeElement DOM) or no
// highlighter is supplied, so the tested Run path is unchanged. Shared by BOTH cell shapes:
// client-built cells (`buildRunnableCell`) and adopted compiler-published cells
// (`adoptPublishedRunnableCell`), so published pages colour identically to the playground.
// SECURITY: `opts.highlight` is set to `innerHTML`, so it MUST HTML-escape its input and
// emit only its own tags. `highlightWitchy` (the only caller-supplied value) does exactly
// that — `escapeHtml` on every text slice, emitting `<span class="t-*">` — and is teeth-
// tested for it (`witchy_highlighter_colours_current_syntax`: metacharacters are escaped,
// no injection). Do NOT pass a highlighter that interpolates raw source into HTML.
function buildEditorSurface(doc, editor, source, opts) {
  const highlight = typeof opts.highlight === "function" ? opts.highlight : null;
  const canHtml = highlight && "innerHTML" in doc.createElement("pre");
  if (!canHtml) return editor;
  const wrap = doc.createElement("div");
  wrap.setAttribute("class", "witchy-editor-wrap");
  const painted = doc.createElement("pre");
  painted.setAttribute("class", "witchy-highlight");
  painted.setAttribute("aria-hidden", "true");
  // Trailing "\n" so a final empty line in the textarea has matching height in the pre.
  const paint = () => {
    painted.innerHTML = highlight(typeof editor.value === "string" ? editor.value : source) + "\n";
  };
  paint();
  if (typeof editor.addEventListener === "function") editor.addEventListener("input", paint);
  wrap.appendChild(painted);
  wrap.appendChild(editor);
  return wrap;
}

function hasClass(node, name) {
  return classOf(node).split(/\s+/).includes(name);
}

function findByClass(root, name, acc = []) {
  if (hasClass(root, name)) acc.push(root);
  for (const child of root.childNodes || []) findByClass(child, name, acc);
  return acc;
}

function findPublishedRunnableCells(root, acc = []) {
  if (
    typeof root.getAttribute === "function"
    && root.getAttribute("data-witchy-runnable") === "1"
  ) {
    if (!adoptedRunnableCells.has(root)) acc.push(root);
    return acc;
  }
  for (const child of root.childNodes || []) findPublishedRunnableCells(child, acc);
  return acc;
}

// Collect every ordinary Witchy code fence and every compiler-emitted runnable
// host marker under `root`.
function findWitchyCells(root, acc = []) {
  const kids = root.childNodes || [];
  for (const child of kids) {
    const pre = child.parentNode;
    const compilerRunnable =
      pre && typeof pre.getAttribute === "function"
      && pre.getAttribute("data-glamour-slot-kind") === "witchy-runnable";
    if (
      isCode(child)
      && (classOf(child).split(/\s+/).includes("language-witchy") || compilerRunnable)
    ) {
      if (pre) acc.push({ pre, code: child });
    } else {
      findWitchyCells(child, acc);
    }
  }
  return acc;
}

/**
 * A small "Copy" button that writes `getText()` to the clipboard, with brief visual feedback.
 * Browser-only (uses `navigator.clipboard`); under the headless DOM it has no listener and is an
 * inert affordance, so the tested build path is unchanged. `getText` is read AT CLICK TIME so a
 * runnable cell copies the reader's current edits, not just the seed.
 */
function installCopyBehavior(btn, getText) {
  if (typeof btn.addEventListener === "function") {
    btn.addEventListener("click", async () => {
      try {
        if (!(navigator && navigator.clipboard && navigator.clipboard.writeText)) {
          throw new Error("clipboard unavailable");
        }
        await navigator.clipboard.writeText(getText());
        btn.textContent = "Copied!";
      } catch {
        btn.textContent = "Copy failed";
      }
      setTimeout(() => { btn.textContent = "Copy"; }, 1200);
    });
  }
}

function buildCopyButton(doc, getText) {
  const btn = doc.createElement("button");
  btn.setAttribute("class", "witchy-copy");
  btn.setAttribute("type", "button");
  btn.setAttribute("aria-label", "Copy code to clipboard");
  btn.textContent = "Copy";
  installCopyBehavior(btn, getText);
  return btn;
}

function adoptPublishedRunnableCell(element, opts) {
  const required = ["witchy-editor", "witchy-run", "witchy-output", "witchy-stats", "witchy-copy"];
  const parts = Object.fromEntries(required.map((name) => {
    const matches = findByClass(element, name);
    if (matches.length !== 1) {
      throw new Error(`witchy-runnable: published cell requires one ${name}`);
    }
    return [name, matches[0]];
  }));
  const editor = parts["witchy-editor"];
  const runButton = parts["witchy-run"];
  const output = parts["witchy-output"];
  const statsOutput = parts["witchy-stats"];
  const copyButton = parts["witchy-copy"];
  const source = typeof editor.value === "string" && editor.value !== ""
    ? editor.value
    : editor.textContent || "";
  editor.value = source;

  // Overlay the published textarea with the same highlight surface client-built
  // cells get. The editor node is REUSED (listeners, value, identity retained);
  // only its position in the cell moves inside the wrap.
  const doc = element.ownerDocument
    || (typeof document !== "undefined" ? document : null);
  // Capture the editor's slot BEFORE buildEditorSurface re-parents it into the wrap.
  const host = editor.parentNode;
  const slot = editor.nextSibling;
  const surface = doc ? buildEditorSurface(doc, editor, source, opts) : editor;
  if (surface !== editor && host && typeof host.insertBefore === "function") {
    host.insertBefore(surface, slot);
  }

  const run = async () => {
    const current = typeof editor.value === "string" ? editor.value : source;
    runButton.textContent = "Running…";
    try {
      const { ok, text, stats } = await opts.runProgram(current, opts.runOptions || {});
      output.textContent = text || (ok ? "(no output)" : "(empty error)");
      output.setAttribute("class", ok ? "witchy-output ok" : "witchy-output err");
      statsOutput.textContent = /^\s*mode\s+opt\s*$/m.test(current)
        ? formatOptimizationStats(stats)
        : "";
    } catch (error) {
      output.textContent = "playground error: " + ((error && error.message) || error);
      output.setAttribute("class", "witchy-output err");
      statsOutput.textContent = "";
    }
    runButton.textContent = "Run";
  };
  if (typeof runButton.addEventListener === "function") runButton.addEventListener("click", run);
  if (typeof editor.addEventListener === "function") {
    editor.addEventListener("keydown", (event) => {
      if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
        if (typeof event.preventDefault === "function") event.preventDefault();
        run();
      }
    });
  }
  installCopyBehavior(copyButton, () => (
    typeof editor.value === "string" ? editor.value : source
  ));
  adoptedRunnableCells.add(element);
  return { element, editor, runButton, output, statsOutput, run };
}

/**
 * Build a runnable cell for `source`: a `<pre>` showing the code, a Run button, and an output
 * pane. On Run, delegate the current source to the caller's explicit execution boundary.
 *
 * @param {Document} doc
 * @param {string} source
 * @param {object} opts
 * @param {function} opts.runProgram async `(source, runOptions) => result`.
 *   The production docs pass the opaque-iframe runner from `witchy-cell-sandbox.js`.
 * @returns {{ element, editor, runButton, output, statsOutput, run }}
 */
export function buildRunnableCell(doc, source, opts = {}) {
  if (typeof opts.runProgram !== "function") {
    throw new Error("witchy-runnable: opts.runProgram must be an async program runner");
  }

  const element = doc.createElement("div");
  element.setAttribute("class", "witchy-cell");
  // The source is EDITABLE: a `<textarea>` seeded with the block. Because the docs app mounts
  // the cell in a non-diffed host Slot, an edit survives page re-renders. The textarea carries
  // no `language-witchy` class, so a standalone re-scan (`enhanceRunnableCells`) never
  // re-enhances an already-built cell.
  const editor = doc.createElement("textarea");
  editor.setAttribute("class", "witchy-editor");
  editor.setAttribute("spellcheck", "false");
  // Size the box to the code so the whole program shows without scrolling. `rows` needs
  // no layout (works even headless); the highlight overlay below grows further to fit.
  editor.setAttribute("rows", String(Math.max(3, source.split("\n").length)));
  editor.value = source;

  const runButton = doc.createElement("button");
  runButton.setAttribute("class", "witchy-run");
  runButton.textContent = "Run";
  const output = doc.createElement("pre");
  output.setAttribute("class", "witchy-output");
  const statsOutput = doc.createElement("pre");
  statsOutput.setAttribute("class", "witchy-stats");

  const run = async () => {
    // Run the CURRENT editor contents (the reader's edits), falling back to the seed source.
    const src = typeof editor.value === "string" ? editor.value : source;
    runButton.textContent = "Running…";
    try {
      const { ok, text, stats } = await opts.runProgram(src, opts.runOptions || {});
      output.textContent = text || (ok ? "(no output)" : "(empty error)");
      output.setAttribute("class", ok ? "witchy-output ok" : "witchy-output err");
      // `mode opt` makes resource behavior part of the checked source contract.
      // Surface the compiled module's exact counters for those examples while
      // keeping ordinary introductory cells focused on program output.
      statsOutput.textContent = /^\s*mode\s+opt\s*$/m.test(src)
        ? formatOptimizationStats(stats)
        : "";
    } catch (e) {
      output.textContent = "playground error: " + ((e && e.message) || e);
      output.setAttribute("class", "witchy-output err");
      statsOutput.textContent = "";
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

  // (RFC-0041) Optional syntax highlighting: overlay the editable textarea on a `<pre>`
  // painted by `opts.highlight` (an XSS-safe `source -> HTML`, e.g. `highlightWitchy`). The
  // pre is in normal flow and SIZES the cell — it grows to fit every line, so the whole
  // program is always visible (no cutoff). The textarea sits transparently over it (caret
  // + selection show; its text is transparent so the coloured pre shows through). Both share
  // identical type metrics (CSS) so the caret aligns. Degrades to a plain textarea where
  // `innerHTML` is unavailable (the headless FakeElement DOM) or no highlighter is supplied,
  // so the tested Run path is unchanged.
  // SECURITY: `opts.highlight` is set to `innerHTML`, so it MUST HTML-escape its input and
  // emit only its own tags. `highlightWitchy` (the only caller-supplied value) does exactly
  // that — `escapeHtml` on every text slice, emitting `<span class="t-*">` — and is teeth-
  // tested for it (`witchy_highlighter_colours_current_syntax`: metacharacters are escaped,
  // no injection). Do NOT pass a highlighter that interpolates raw source into HTML.
  element.appendChild(buildEditorSurface(doc, editor, source, opts));
  element.appendChild(runButton);
  element.appendChild(output);
  element.appendChild(statsOutput);
  // A copy affordance over the code — reads the live editor value at click time.
  element.appendChild(buildCopyButton(doc, () => (typeof editor.value === "string" ? editor.value : source)));
  return { element, editor, runButton, output, statsOutput, run };
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
 * A glamour host-slot renderer for NON-runnable witchy code — a capability example that can't
 * execute in the capability-denied cell (`Dir`/`Net`/… would just error). It renders a
 * syntax-highlighted, read-only `<pre>`: no textarea, no Run button, so no misleading error,
 * while the code still reads clearly. SECURITY: `opts.highlight` is set to `innerHTML`, so it
 * MUST HTML-escape its input (`highlightWitchy` does — see the note in `buildRunnableCell`).
 */
export function staticSlot(opts = {}) {
  const highlight = typeof opts.highlight === "function" ? opts.highlight : null;
  return (doc, source) => {
    const pre = doc.createElement("pre");
    pre.setAttribute("class", "witchy-static");
    const code = doc.createElement("code");
    if (highlight && "innerHTML" in code) {
      code.innerHTML = highlight(source);
    } else {
      code.appendChild(doc.createTextNode(source));
    }
    pre.appendChild(code);
    pre.appendChild(buildCopyButton(doc, () => source));
    return pre;
  };
}

/**
 * Standalone progressive enhancement (a static page / the playground): find every
 * `pre>code.language-witchy` or a compiler-emitted `witchy-runnable` host marker
 * under `root` and REPLACE it with a runnable cell. Idempotent and DOM-agnostic.
 */
const STATIC_SLOT_KINDS = new Set(["witchy-static", "shell-static", "toml-static"]);

/**
 * Paint compiler-emitted static highlight slots (`data-glamour-slot-kind`) and
 * `pre>code.language-witchy-static` fences. Used on published pages after the
 * host loader starts. Does not create Run cells.
 */
export function enhanceStaticSlots(root, opts = {}) {
  const highlight = typeof opts.highlight === "function" ? opts.highlight : null;
  const highlightShell = typeof opts.highlightShell === "function" ? opts.highlightShell : null;
  const highlightToml = typeof opts.highlightToml === "function" ? opts.highlightToml : null;
  const paint = (node, fn, source) => {
    if (!fn || !("innerHTML" in node)) {
      if (!node.childNodes || node.childNodes.length === 0) {
        node.appendChild((opts.document || node.ownerDocument).createTextNode(source));
      }
      return;
    }
    node.innerHTML = fn(source);
  };
  const visit = (node) => {
    if (typeof node.getAttribute === "function") {
      const kind = node.getAttribute("data-glamour-slot-kind");
      if (STATIC_SLOT_KINDS.has(kind)) {
        const source = node.textContent || "";
        const code = node.childNodes && node.childNodes[0];
        const target = code && isCode(code) ? code : node;
        if (kind === "witchy-static") paint(target, highlight, source);
        else if (kind === "shell-static") paint(target, highlightShell, source);
        else if (kind === "toml-static") paint(target, highlightToml, source);
        return;
      }
    }
    if (isCode(node) && classOf(node).split(/\s+/).includes("language-witchy-static")) {
      paint(node, highlight, node.textContent || "");
      return;
    }
    for (const child of node.childNodes || []) visit(child);
  };
  visit(root);
}

export function enhanceRunnableCells(root, opts = {}) {
  const doc = opts.document || (typeof document !== "undefined" ? document : null);
  if (!doc) throw new Error("witchy-runnable: no `document` (pass opts.document)");
  if (typeof opts.runProgram !== "function") {
    throw new Error("witchy-runnable: opts.runProgram must be an async program runner");
  }
  const cells = findPublishedRunnableCells(root).map((element) => ({
    pre: null,
    code: null,
    ...adoptPublishedRunnableCell(element, opts),
  }));
  for (const { pre, code } of findWitchyCells(root)) {
    const cell = buildRunnableCell(doc, code.textContent, opts);
    const parent = pre.parentNode;
    if (parent) parent.replaceChild(cell.element, pre);
    cells.push({ pre, code, ...cell });
  }
  return cells;
}

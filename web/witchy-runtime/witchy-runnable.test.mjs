#!/usr/bin/env node
// RFC-0041 Phase 2: the runnable cell, end to end and headless. Builds a fake DOM with a
// `<pre><code class="language-witchy">…</code></pre>` block (exactly what `markdown.to_vnode`
// now emits), enhances it with `enhanceRunnableCells`, and DRIVES Run — which compiles+runs
// the block's source with the SAME `witchy-host.js` engine + `web/witchy.wasm` the validated
// playground uses. Asserts the output equals the program's, that a compile error surfaces as
// an error cell, and that enhancement is idempotent. No browser needed: `witchy-host.js` runs
// under Node/V8 (that is what `pg_validate.mjs` uses).
//
// Usage:  node web/witchy-runtime/witchy-runnable.test.mjs

import { enhanceRunnableCells } from "../witchy-runnable.js";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");

// A minimal DOM, matching the glamour drivers' FakeElement shape.
class FakeNode {
  constructor() { this.childNodes = []; this.parentNode = null; }
  appendChild(c) { if (c.parentNode) c.parentNode.removeChild(c); c.parentNode = this; this.childNodes.push(c); return c; }
  removeChild(c) { const i = this.childNodes.indexOf(c); if (i >= 0) this.childNodes.splice(i, 1); c.parentNode = null; return c; }
  replaceChild(n, p) { const i = this.childNodes.indexOf(p); if (i < 0) throw new Error("replaceChild"); this.childNodes[i] = n; n.parentNode = this; p.parentNode = null; return p; }
}
class FakeText extends FakeNode {
  constructor(t) { super(); this._t = t; }
  get textContent() { return this._t; }
  set textContent(v) { this._t = v; this.childNodes = []; }
}
class FakeElement extends FakeNode {
  constructor(tag) { super(); this.el = tag; this.attributes = new Map(); this.listeners = new Map(); }
  setAttribute(n, v) { this.attributes.set(n, String(v)); }
  getAttribute(n) { return this.attributes.has(n) ? this.attributes.get(n) : null; }
  addEventListener(e, fn) { if (!this.listeners.has(e)) this.listeners.set(e, new Set()); this.listeners.get(e).add(fn); }
  dispatchEvent(ev) { const s = this.listeners.get(ev.type); if (s) for (const fn of [...s]) fn(ev); return true; }
  get textContent() { let o = ""; for (const c of this.childNodes) o += c.textContent; return o; }
  set textContent(v) { this.childNodes = []; this.appendChild(new FakeText(v)); }
}
const doc = { createElement: (t) => new FakeElement(t), createTextNode: (t) => new FakeText(t) };
const qsa = (node, tag, acc = []) => {
  if (node instanceof FakeElement && node.el === tag) acc.push(node);
  for (const c of node.childNodes) if (c instanceof FakeElement) qsa(c, tag, acc);
  return acc;
};

// Build `<div class=markdown><pre><code class="language-witchy">SOURCE</code></pre></div>`.
function pageWith(source) {
  const div = new FakeElement("div");
  div.setAttribute("class", "markdown");
  const pre = new FakeElement("pre");
  const code = new FakeElement("code");
  code.setAttribute("class", "language-witchy");
  code.appendChild(new FakeText(source));
  pre.appendChild(code);
  div.appendChild(pre);
  return div;
}

// The compiler: instantiate `web/witchy.wasm` (no imports) — the very module the playground
// and pg_validate use. Lazily, exactly as a browser would fetch it on first Run.
const loadCompiler = async () => {
  const bytes = readFileSync(resolve(REPO, "web/witchy.wasm"));
  const { instance } = await WebAssembly.instantiate(bytes, {});
  return instance.exports;
};

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

try {
  // 1. A runnable cell: enhance, then Run, and the real output shows.
  const root = pageWith('fn main(console: Console):\n    print(console, "hello from a runnable cell")');
  const cells = enhanceRunnableCells(root, { document: doc, loadCompiler });
  ok(cells.length === 1, "one runnable cell is found and enhanced");
  ok(qsa(root, "button").some((b) => (b.getAttribute("class") || "").includes("witchy-run")), "a Run button is added");
  ok(qsa(root, "div").some((d) => (d.getAttribute("class") || "") === "witchy-cell"), "the cell is wrapped");
  ok(qsa(root, "textarea").length === 1 && cells[0].editor.value.includes("hello from a runnable cell"), "the cell is EDITABLE (a textarea seeded with the source)");

  await cells[0].run();
  ok(cells[0].output.textContent === "hello from a runnable cell", "Run compiles + runs the code and shows its output");
  ok((cells[0].output.getAttribute("class") || "").includes("ok"), "a successful run is marked ok");

  // Editing the textarea and re-running runs the READER'S edited source, not the seed.
  cells[0].editor.value = 'fn main(console: Console):\n    print(console, "the reader edited this")';
  await cells[0].run();
  ok(cells[0].output.textContent === "the reader edited this", "Run executes the reader's EDITED source");

  // 2. Idempotent: re-enhancing the same root finds nothing new.
  const again = enhanceRunnableCells(root, { document: doc, loadCompiler });
  ok(again.length === 0, "re-enhancing is idempotent (no double cells)");

  // 3. A compile error surfaces as an error cell, not a thrown exception.
  const bad = pageWith("fn main(console: Console):\n    print(console, nope)");
  const badCells = enhanceRunnableCells(bad, { document: doc, loadCompiler });
  await badCells[0].run();
  ok((badCells[0].output.getAttribute("class") || "").includes("err"), "a compile error marks the cell err");
  ok(badCells[0].output.textContent.length > 0, "the error message is shown");

  // 4. A non-witchy code block is left alone.
  const other = new FakeElement("div");
  const pre = new FakeElement("pre");
  const code = new FakeElement("code");
  code.setAttribute("class", "language-sh");
  code.appendChild(new FakeText("echo hi"));
  pre.appendChild(code); other.appendChild(pre);
  ok(enhanceRunnableCells(other, { document: doc, loadCompiler }).length === 0, "a non-witchy fence is not enhanced");
} catch (e) {
  console.error("harness threw:", e);
  failures++;
}

if (failures > 0) {
  console.error(`\nWITCHY-RUNNABLE FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nWITCHY-RUNNABLE OK");
process.exit(0);

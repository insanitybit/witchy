#!/usr/bin/env node
// RFC-0008 dogfooding test: drive the glamour SYNTAX HIGHLIGHTER rune headlessly.
// This is the proving ground for coven-web's sandbox highlighter migration
// (projects/coven-web/PLAN.md WS-I/M6): the witchy rune takes untrusted package
// SOURCE TEXT and produces a glamour `VNode` tree of classed `<span>` runs, and
// the JS shell renders that tree to DOM with createElement/textContent ONLY.
//
// We:
//   1. compile the `highlighter` rune to WASM via the real `witchy` binary
//      (with glamour as a sibling so the import resolves);
//   2. call its `export_render({src})` export through the RFC-0007 pure-compute
//      host shim (web/witchy-runtime/witchy-runtime.mjs) — the rune is capability-
//      denied, so it instantiates only because its footprint is empty;
//   3. parse the returned VNode JSON and render it into a self-contained fake DOM
//      via the SAME createElement/textContent/setAttribute walk the glamour-dom
//      shell uses (no HTML-string sink);
//   4. assert the highlighted structure: a `pre>code`, keyword `fn`/`print` in
//      `span.kw`, the comment in `span.com`, the string in `span.str`; and
//   5. assert XSS-IMMUNITY: a snippet containing `<script>` renders as ESCAPED
//      text (a real DOM text node, NEVER a live <script> element).
//
// Usage:  node web/witchy-runtime/highlighter.test.mjs [path/to/witchy-binary]
// Exit 0 on success; non-zero (with a message) on any failure. Node is the host
// engine; the Rust wrapper SKIPS cleanly if node is absent.

import { instantiate } from "./witchy-runtime.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, copyFileSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");

// --- a tiny fake DOM (mirrors glamour-dom.test.mjs: just the surface the render
// walk touches) -------------------------------------------------------------
class FakeNode {
  constructor() {
    this.childNodes = [];
    this.parentNode = null;
  }
  appendChild(child) {
    child.parentNode = this;
    this.childNodes.push(child);
    return child;
  }
}

class FakeText extends FakeNode {
  constructor(text) {
    super();
    this.nodeType = 3; // Node.TEXT_NODE
    this._text = text;
  }
  get textContent() {
    return this._text;
  }
}

class FakeElement extends FakeNode {
  constructor(tag) {
    super();
    this.nodeType = 1; // Node.ELEMENT_NODE
    this.tagName = tag.toUpperCase();
    this.el = tag;
    this.attributes = new Map();
  }
  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }
  getAttribute(name) {
    return this.attributes.has(name) ? this.attributes.get(name) : null;
  }
  // textContent of an element is the concatenation of its descendants' text.
  get textContent() {
    let out = "";
    for (const c of this.childNodes) out += c.textContent;
    return out;
  }
}

const fakeDocument = {
  createElement: (tag) => new FakeElement(tag),
  createTextNode: (text) => new FakeText(text),
};

// Render a VNode (wire JSON) into a DOM node using createElement / textContent /
// setAttribute ONLY — the SAME structurally-injection-free walk glamour-dom's
// `createNode` uses. There is NO HTML-string sink (no innerHTML), so a `text`
// node's content can never become markup: that is the XSS-immunity this test
// asserts below. (The highlighter emits no `on` bindings, so events are omitted.)
function renderVNode(doc, v) {
  if (v != null && typeof v.text === "string") return doc.createTextNode(v.text);
  if (v == null || typeof v.el !== "string") {
    throw new Error(`malformed vnode: ${JSON.stringify(v)}`);
  }
  const el = doc.createElement(v.el);
  for (const [kind, name, value] of v.attrs || []) {
    if (kind === "prop") el.setAttribute(name, value);
    else throw new Error(`unexpected attr kind \`${kind}\` (highlighter emits only props)`);
  }
  for (const k of v.kids || []) el.appendChild(renderVNode(doc, k));
  return el;
}

// Find the first descendant element with the given tag.
function querySelector(node, tag) {
  if (node instanceof FakeElement && node.el === tag) return node;
  for (const c of node.childNodes) {
    const found = querySelector(c, tag);
    if (found) return found;
  }
  return null;
}

// All descendant elements (document order) for which `pred` holds.
function findAll(node, pred, acc = []) {
  if (node instanceof FakeElement && pred(node)) acc.push(node);
  for (const c of node.childNodes) {
    if (c instanceof FakeElement) findAll(c, pred, acc);
  }
  return acc;
}

const hasClass = (cls) => (el) => el.el === "span" && el.getAttribute("class") === cls;

// --- the test ---------------------------------------------------------------
let failures = 0;
const ok = (cond, msg) => {
  console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`);
  if (!cond) failures++;
};

const work = mkdtempSync(join(tmpdir(), "highlighter-"));
try {
  // Compile the highlighter rune (glamour as a sibling so the import resolves).
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  copyFileSync(join(REPO, "projects/glamour/src/highlight.witchy"), join(work, "highlight.witchy"));
  copyFileSync(
    join(REPO, "projects/glamour/examples/highlighter/src/highlighter.witchy"),
    join(work, "highlighter.witchy"),
  );
  const wasmPath = join(work, "highlighter.wasm");
  execFileSync(BIN, ["compile", join(work, "highlighter.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  // Instantiate under the pure-compute host — it links ONLY because the rune's
  // footprint is empty (a capability-using rune would fail with a LinkError here).
  const { callString } = await instantiate(wasm);

  // render({src}) -> the parsed VNode tree, rendered into the fake DOM.
  const render = (src) => {
    const out = callString("__export_export_render", JSON.stringify({ src }));
    const tree = JSON.parse(out);
    ok(!tree.error, `export_render returns a VNode (not an error) for src \`${src.slice(0, 20)}…\``);
    return renderVNode(fakeDocument, tree);
  };

  // --- a representative witchy snippet ---
  const snippet = 'fn main(c: Console):\n    // hi\n    print(c, "x")';
  const root = render(snippet);

  // Structure: a <pre> wrapping a <code>.
  const pre = querySelector(root, "pre");
  ok(pre !== null, "the highlighter renders a <pre> root");
  const code = pre && querySelector(pre, "code");
  ok(code !== null, "the <pre> wraps a <code>");

  // Keywords `fn` and `print`... wait: `print` is NOT a keyword in witchy (it is a
  // function), so only true keywords get span.kw. Assert `fn` is a keyword span,
  // and `print` is NOT (it renders as plain text) — proving the classifier is real,
  // not blanket-wrapping every identifier.
  const kwSpans = findAll(root, hasClass("kw"));
  const kwTexts = kwSpans.map((s) => s.textContent);
  ok(kwTexts.includes("fn"), "the keyword `fn` is wrapped in span.kw");
  ok(!kwTexts.includes("print"), "the function `print` is NOT a keyword (plain text), classifier is selective");

  // The whole `// hi` comment is one span.com.
  const comSpans = findAll(root, hasClass("com"));
  ok(comSpans.length === 1 && comSpans[0].textContent === "// hi", "the line comment is wrapped in span.com");

  // The string literal `"x"` (quotes included) is one span.str.
  const strSpans = findAll(root, hasClass("str"));
  ok(strSpans.length === 1 && strSpans[0].textContent === '"x"', 'the string literal "x" is wrapped in span.str');

  // The full rendered text round-trips the source exactly (no characters lost or
  // duplicated by the tokenizer).
  ok(root.textContent === snippet, "the highlighted text reconstructs the original source exactly");

  // --- the XSS-immunity check: a `<script>` in the source renders as INERT TEXT,
  //     never a real element. The highlighter routes every token through a
  //     text(...) node, so the `<` `>` are plain characters in a DOM text node;
  //     there is no <script> element and no markup sink. ---
  const evil = 'let x = "<script>alert(1)</script>"';
  const evilRoot = render(evil);
  // No real <script> element exists anywhere in the tree.
  const scriptEls = findAll(evilRoot, (el) => el.el === "script");
  ok(scriptEls.length === 0, "a <script> in the source produces NO real <script> element (XSS-immune)");
  // The angle brackets survive as literal text inside the rendered DOM.
  ok(evilRoot.textContent.includes("<script>"), "the `<script>` text is preserved verbatim as inert text");
  // And it landed inside the string span (it is part of a string literal here),
  // confirming the bytes are a text node, not parsed markup.
  const evilStr = findAll(evilRoot, hasClass("str"));
  ok(
    evilStr.length === 1 && evilStr[0].textContent.includes("<script>alert(1)</script>"),
    "the injected markup is contained as plain text inside the string span",
  );
  // Every leaf carrying the `<script>` text is a TEXT node (nodeType 3), proving
  // it is content, not structure.
  const textLeaves = [];
  const collectText = (n) => {
    if (n.nodeType === 3) textLeaves.push(n);
    for (const c of n.childNodes) collectText(c);
  };
  collectText(evilRoot);
  ok(
    textLeaves.some((t) => t.textContent.includes("<script>")),
    "the `<script>` bytes live in a DOM TEXT node (content), never an element (structure)",
  );
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nHIGHLIGHTER TEST FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nHIGHLIGHTER OK");

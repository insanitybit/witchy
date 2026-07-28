#!/usr/bin/env node
// RFC-0015 Phase D: the coven package-DETAIL page, built on glamour. Mounts the
// `package_page` example and asserts it composes the registry's key view: the package
// identity (name, version, copy-able install command), the capability FOOTPRINT as badges
// (coven's differentiator), and the README + generated API docs rendered from Markdown via
// `markdown.to_vnode` — with a README/Docs tab toggle driving the MVU loop. This is the
// template the coven-web shell's package view is built on.
//
// Usage:  node web/witchy-runtime/glamour-package-page.test.mjs [path/to/witchy-binary]

import { mount } from "./glamour-dom.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, copyFileSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");

class FakeNode {
  constructor() {
    this.childNodes = [];
    this.parentNode = null;
  }
  appendChild(c) {
    if (c.parentNode) c.parentNode.removeChild(c);
    c.parentNode = this;
    this.childNodes.push(c);
    return c;
  }
  removeChild(c) {
    const i = this.childNodes.indexOf(c);
    if (i >= 0) this.childNodes.splice(i, 1);
    c.parentNode = null;
    return c;
  }
  replaceChild(n, p) {
    const i = this.childNodes.indexOf(p);
    if (i < 0) throw new Error("replaceChild: old node not found");
    this.childNodes[i] = n;
    n.parentNode = this;
    p.parentNode = null;
    return p;
  }
}
class FakeText extends FakeNode {
  constructor(t) {
    super();
    this._t = t;
  }
  get textContent() {
    return this._t;
  }
  set textContent(v) {
    this._t = v;
    this.childNodes = [];
  }
}
class FakeElement extends FakeNode {
  constructor(tag) {
    super();
    this.el = tag;
    this.attributes = new Map();
    this.listeners = new Map();
  }
  setAttribute(n, v) {
    this.attributes.set(n, String(v));
  }
  getAttribute(n) {
    return this.attributes.has(n) ? this.attributes.get(n) : null;
  }
  removeAttribute(n) {
    this.attributes.delete(n);
  }
  addEventListener(e, fn) {
    if (!this.listeners.has(e)) this.listeners.set(e, new Set());
    this.listeners.get(e).add(fn);
  }
  removeEventListener(e, fn) {
    const s = this.listeners.get(e);
    if (s) s.delete(fn);
  }
  dispatchEvent(ev) {
    const s = this.listeners.get(ev.type);
    if (s) for (const fn of [...s]) fn(ev);
    return true;
  }
  get textContent() {
    let o = "";
    for (const c of this.childNodes) o += c.textContent;
    return o;
  }
}
const fakeDocument = {
  createElement: (t) => new FakeElement(t),
  createTextNode: (t) => new FakeText(t),
};
function querySelector(node, tag) {
  if (node instanceof FakeElement && node.el === tag) return node;
  for (const c of node.childNodes) {
    const f = c instanceof FakeElement || c instanceof FakeText ? querySelector(c, tag) : null;
    if (f) return f;
  }
  return null;
}
function querySelectorAll(node, tag, acc = []) {
  if (node instanceof FakeElement && node.el === tag) acc.push(node);
  for (const c of node.childNodes) if (c instanceof FakeElement) querySelectorAll(c, tag, acc);
  return acc;
}

let failures = 0;
const ok = (cond, msg) => {
  console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`);
  if (!cond) failures++;
};

const work = mkdtempSync(join(tmpdir(), "glamour-pkg-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  copyFileSync(join(REPO, "projects/glamour/src/markdown.witchy"), join(work, "markdown.witchy"));
  copyFileSync(join(REPO, "projects/glamour/examples/package_page/src/package_page.witchy"), join(work, "package_page.witchy"));
  const wasmPath = join(work, "package_page.wasm");
  execFileSync(BIN, ["compile", join(work, "package_page.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  const root = new FakeElement("root");
  await mount(wasm, root, { document: fakeDocument, initialModel: 0 });

  // Package identity.
  ok(querySelector(root, "h1").textContent === "acme/charts", "the package name renders as <h1>");
  ok(root.textContent.includes("v1.2.0"), "the version renders");
  ok(querySelector(root, "pre").textContent === "witchy add acme/charts", "a copy-able install command renders");

  // Capability footprint as badges (coven's differentiator).
  const chips = querySelectorAll(root, "span").filter((s) => (s.getAttribute("class") || "").includes("chip cap"));
  const chipText = chips.map((c) => c.textContent);
  ok(chipText.includes("Net[Connect, Tls]"), "the network capability renders as a footprint badge");
  ok(chipText.includes("Js{d3-runes-chart}"), "the foreign-code (Js compartment) capability renders as a badge");

  // README (untrusted Markdown) renders inline: bold + prose, safe by construction.
  ok(querySelectorAll(root, "strong").length >= 1, "the README's **bold** renders as <strong>");
  ok(root.textContent.includes("XSS-immune"), "the README prose renders");
  // The README's link href is scheme-safe (markdown sanitizes; no javascript: anywhere).
  ok(querySelectorAll(root, "a").every((a) => !(a.getAttribute("href") || "").startsWith("javascript:")), "no unsafe link href is rendered");

  // Switch to the API-docs tab -> the generated docs render.
  ok(!root.textContent.includes("Render a bar chart"), "API docs are hidden under the README tab initially");
  const docsTab = querySelectorAll(root, "button").find((b) => b.textContent === "API docs");
  ok(!!docsTab, "an `API docs` tab button is present");
  docsTab.dispatchEvent({ type: "click" });
  ok(root.textContent.includes("Render a bar chart"), "the API docs render after switching tabs");
  ok(root.textContent.includes("fn bar(data: List(Int))"), "the generated function signature renders in the docs tab");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-PACKAGE-PAGE FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-PACKAGE-PAGE OK");

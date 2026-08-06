#!/usr/bin/env node
// RFC-0015 Phase D: the coven-web SHELL on glamour, end to end and headless. Mounts the
// `coven_app` rune with an INJECTED fake registry (fetch) and history, then drives the real
// app loop: the initial route fetches the catalog and renders the rune list; clicking a rune
// `navigate`s to its package URL, which fetches the docs and renders them. The rune holds no
// Net — the host shell performs every fetch — so this proves the routed, data-fetched
// trusted shell works with authority entirely at the edge.
//
// Usage:  node web/witchy-runtime/glamour-coven-app.test.mjs [path/to/witchy-binary]

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
function querySelectorAll(node, tag, acc = []) {
  if (node instanceof FakeElement && node.el === tag) acc.push(node);
  for (const c of node.childNodes) if (c instanceof FakeElement) querySelectorAll(c, tag, acc);
  return acc;
}
const tick = () => new Promise((r) => setTimeout(r, 0));

let failures = 0;
const ok = (cond, msg) => {
  console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`);
  if (!cond) failures++;
};

const work = mkdtempSync(join(tmpdir(), "glamour-app-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  copyFileSync(join(REPO, "projects/glamour/src/markdown.witchy"), join(work, "markdown.witchy"));
  copyFileSync(join(REPO, "projects/glamour/examples/coven_app/src/coven_app.witchy"), join(work, "coven_app.witchy"));
  const wasmPath = join(work, "coven_app.wasm");
  execFileSync(BIN, ["compile", join(work, "coven_app.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  // The fake registry: a catalog and a per-package docs endpoint.
  const calls = [];
  const fakeFetch = (url) => {
    calls.push(url);
    if (url.includes("/catalog")) {
      return Promise.resolve({ status: 200, text: () => Promise.resolve('{"runes":[{"name":"acme/charts"},{"name":"std/json"}]}') });
    }
    if (url.includes("/doc")) {
      return Promise.resolve({ status: 200, text: () => Promise.resolve('{"markdown":"## acme/charts\\n\\nA charting library for witchy."}') });
    }
    return Promise.resolve({ status: 404, text: () => Promise.resolve("") });
  };
  const location = { pathname: "/" };
  const pushed = [];
  const history = { pushState: (_s, _t, p) => { pushed.push(p); location.pathname = p; } };

  const root = new FakeElement("root");
  await mount(wasm, root, {
    document: fakeDocument,
    initialModel: { route: "/", catalog: "", package: "" },
    fetch: fakeFetch,
    // (RFC-0040) the app's `export_step` takes a `UiRoot`; stage its grant.
    instantiateOpts: { userCaps: [["coven"]] },
    routeTag: "Route",
    location,
    history,
  });

  // The initial route fetched the catalog and rendered the rune list.
  await tick();
  await tick();
  ok(calls.some((u) => u.includes("/api/coven/catalog")), "the initial route fetched the catalog");
  const runeButtons = querySelectorAll(root, "button").filter((b) => (b.getAttribute("class") || "").includes("rune-name"));
  ok(runeButtons.length === 2, "the catalog renders the two runes from the fake registry");
  ok(runeButtons.map((b) => b.textContent).includes("acme/charts"), "a rune name renders");

  // Click a rune -> navigate to its package URL -> fetch + render its docs.
  runeButtons.find((b) => b.textContent === "acme/charts").dispatchEvent({ type: "click" });
  ok(pushed[pushed.length - 1] === "/p/acme/charts", "clicking a rune navigates to its package URL");
  await tick();
  await tick();
  ok(calls.some((u) => u.includes("/api/coven/doc?name=acme/charts")), "the package route fetched the docs");
  ok(root.textContent.includes("A charting library for witchy"), "the package page renders the fetched docs");
  ok(querySelectorAll(root, "h2").length >= 1, "the docs Markdown rendered to real elements (an <h2>)");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-COVEN-APP FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-COVEN-APP OK");
process.exit(0);

#!/usr/bin/env node
// RFC-0041 Phase 1: The witchy Book as a glamour app, end to end and headless. Mounts the
// `docs` app (projects/docs) with an injected fake content server (fetch) + history, then
// drives the real loop: the initial route fetches a page's Markdown and renders it; the
// sidebar lists the book's pages; clicking one navigates to its URL, fetches that page, and
// renders it. The rune holds no Net — the host shell performs every fetch — so this proves
// the docs SITE is a capability-pure witchy program (the dogfood), with authority at the edge.
//
// Usage:  node web/witchy-runtime/glamour-docs.test.mjs [path/to/witchy-binary]

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
  removeAttribute(n) { this.attributes.delete(n); }
  addEventListener(e, fn) { if (!this.listeners.has(e)) this.listeners.set(e, new Set()); this.listeners.get(e).add(fn); }
  removeEventListener(e, fn) { const s = this.listeners.get(e); if (s) s.delete(fn); }
  dispatchEvent(ev) { const s = this.listeners.get(ev.type); if (s) for (const fn of [...s]) fn(ev); return true; }
  get textContent() { let o = ""; for (const c of this.childNodes) o += c.textContent; return o; }
}
const fakeDocument = { createElement: (t) => new FakeElement(t), createTextNode: (t) => new FakeText(t) };
function qsa(node, tag, acc = []) {
  if (node instanceof FakeElement && node.el === tag) acc.push(node);
  for (const c of node.childNodes) if (c instanceof FakeElement) qsa(c, tag, acc);
  return acc;
}
const tick = () => new Promise((r) => setTimeout(r, 0));
const settle = async () => { await tick(); await tick(); };
const clickText = (root, text) => {
  const b = qsa(root, "button").find((x) => x.textContent === text);
  if (!b) throw new Error("no button: " + text);
  b.dispatchEvent({ type: "click" });
};

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

const work = mkdtempSync(join(tmpdir(), "glamour-docs-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  copyFileSync(join(REPO, "projects/glamour/src/markdown.witchy"), join(work, "markdown.witchy"));
  copyFileSync(join(REPO, "projects/docs/src/docs.witchy"), join(work, "docs.witchy"));
  const wasmPath = join(work, "docs.wasm");
  execFileSync(BIN, ["compile", join(work, "docs.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  // The fake content server: `/content/SUMMARY.md` -> the nav source; every other
  // `/content/<slug>.md` -> that page's Markdown.
  const SUMMARY = "# Summary\n\n[The witchy Book](title.md)\n\n- [Introduction](introduction.md)\n- [A Tour of the Language](tour.md)\n- [Capabilities](capabilities.md)\n";
  const calls = [];
  const fakeFetch = (url) => {
    calls.push(url);
    if (url.includes("/content/SUMMARY.md")) {
      return Promise.resolve({ status: 200, text: () => Promise.resolve(SUMMARY) });
    }
    const m = url.match(/\/content\/([^.]+)\.md/);
    const slug = m ? m[1] : "unknown";
    const title = slug.charAt(0).toUpperCase() + slug.slice(1);
    return Promise.resolve({ status: 200, text: () => Promise.resolve(`## ${title}\n\nBody text for the **${slug}** page.`) });
  };
  const location = { pathname: "/" };
  const pushed = [];
  const history = { pushState: (_s, _t, p) => { pushed.push(p); location.pathname = p; } };

  const root = new FakeElement("root");
  await mount(wasm, root, {
    document: fakeDocument,
    initialModel: { route: "/", summary: "", content: "# The witchy Book\n\nWelcome." },
    fetch: fakeFetch,
    routeTag: "Route",
    location,
    history,
    // (RFC-0040) the app's `export_step` takes a `UiRoot`; stage its grant.
    instantiateOpts: { userCaps: [["book"]] },
  });

  // 1. The sidebar is DERIVED from the fetched SUMMARY.md (not hardcoded).
  await settle();
  ok(calls.some((u) => u.includes("/content/SUMMARY.md")), "the app fetches SUMMARY.md for the nav");
  const navButtons = qsa(root, "nav").flatMap((n) => qsa(n, "button"));
  ok(navButtons.length === 4, "the sidebar renders one item per SUMMARY.md link (title + 3 pages)");
  ok(navButtons.map((b) => b.textContent).includes("Capabilities"), "a page title parsed from SUMMARY.md renders");
  ok(navButtons.map((b) => b.textContent).includes("A Tour of the Language"), "a multi-word SUMMARY title parses correctly");
  ok(qsa(root, "h1").some((h) => h.textContent === "The witchy Book"), "the book title renders");

  // 2. The initial route fetched the home page and rendered its Markdown to real elements.
  ok(calls.some((u) => u.includes("/content/introduction.md")), "the initial route fetches the home page");
  ok(qsa(root, "h2").some((h) => h.textContent === "Introduction"), "the fetched Markdown renders to a real <h2>");
  ok(root.textContent.includes("Body text for the"), "the page body renders");

  // 3. Clicking a sidebar page navigates to its URL, fetches it, and renders it.
  clickText(root, "Capabilities");
  ok(pushed[pushed.length - 1] === "/p/capabilities", "clicking a page navigates to its URL");
  await settle();
  ok(calls.some((u) => u.includes("/content/capabilities.md")), "the new route fetches that page");
  ok(qsa(root, "h2").some((h) => h.textContent === "Capabilities"), "the new page's Markdown renders");
  // Markdown safety carried over from std/markdown: bold becomes <strong>, not a raw sink.
  ok(qsa(root, "strong").length >= 1, "inline Markdown (bold) renders to a real <strong>");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-DOCS FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-DOCS OK");
process.exit(0);

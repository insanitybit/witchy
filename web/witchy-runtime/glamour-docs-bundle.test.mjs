#!/usr/bin/env node
// RFC-0041: the DEPLOYABLE bundle, validated against the REAL book. Runs `scripts/build-docs.sh`
// to assemble the static site (the docs app compiled to wasm + the real book content + the web
// modules + the manifest), then mounts the bundle's `docs.wasm` with a fetch that reads the
// bundle's staged `content/` — so this proves the actual deploy artifact renders the ACTUAL book
// (real `SUMMARY.md` nav, real pages, real `witchy` fences → runnable cells), not fakes. No
// browser needed: the mount is the tested docs path with a file-backed fetch.
//
// Usage:  node web/witchy-runtime/glamour-docs-bundle.test.mjs [path/to/witchy-binary]

import { mount } from "./glamour-dom.mjs";
import { runnableSlot } from "../witchy-runnable.js";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, readFileSync, existsSync } from "node:fs";
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
  addEventListener(e, fn) { if (!this.listeners.has(e)) this.listeners.set(e, new Set()); this.listeners.get(e).add(fn); }
  removeEventListener(e, fn) { const s = this.listeners.get(e); if (s) s.delete(fn); }
  dispatchEvent(ev) { const s = this.listeners.get(ev.type); if (s) for (const fn of [...s]) fn(ev); return true; }
  get textContent() { let o = ""; for (const c of this.childNodes) o += c.textContent; return o; }
  set textContent(v) { this.childNodes = []; this.appendChild(new FakeText(v)); }
}
const fakeDocument = { createElement: (t) => new FakeElement(t), createTextNode: (t) => new FakeText(t) };
const qsa = (node, tag, acc = []) => {
  if (node instanceof FakeElement && node.el === tag) acc.push(node);
  for (const c of node.childNodes) if (c instanceof FakeElement) qsa(c, tag, acc);
  return acc;
};
const settle = async () => { await new Promise((r) => setTimeout(r, 0)); await new Promise((r) => setTimeout(r, 0)); };

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

const dist = mkdtempSync(join(tmpdir(), "witchy-dist-"));
try {
  // 1. Build the deployable bundle with the docs app pointed at the REAL book.
  execFileSync("bash", [join(REPO, "scripts/build-docs.sh"), dist], {
    cwd: REPO,
    env: { ...process.env, WITCHY: BIN },
    stdio: "pipe",
  });
  for (const f of ["index.html", "docs.wasm", "glamour-dom.mjs", "witchy-runnable.js", "witchy-host.js", "docs-boot.js", "examples.json", "_headers", "content/SUMMARY.md", "content/introduction.md"]) {
    ok(existsSync(join(dist, f)), `the bundle contains ${f}`);
  }
  // The bundle carries strict cross-origin isolation for a `_headers`-honoring host.
  const headers = readFileSync(join(dist, "_headers"), "utf8");
  ok(/Cross-Origin-Opener-Policy:\s*same-origin/.test(headers) && /Cross-Origin-Embedder-Policy:\s*require-corp/.test(headers), "the bundle ships strict COOP/COEP headers");

  // 2. Mount the bundle's docs.wasm; fetch reads the bundle's staged `content/`.
  const wasm = readFileSync(join(dist, "docs.wasm"));
  const fetchCalls = [];
  const fakeFetch = (url) => {
    fetchCalls.push(url);
    const rel = url.startsWith("/") ? url.slice(1) : url; // /content/x.md -> content/x.md
    const path = join(dist, rel);
    if (!existsSync(path)) return Promise.resolve({ status: 404, text: () => Promise.resolve("") });
    return Promise.resolve({ status: 200, text: () => Promise.resolve(readFileSync(path, "utf8")) });
  };
  const location = { pathname: "/" };
  const history = { pushState: (_s, _t, p) => { location.pathname = p; } };
  const root = new FakeElement("root");
  await mount(wasm, root, {
    document: fakeDocument,
    initialModel: { route: "/", summary: "", content: "" },
    fetch: fakeFetch,
    routeTag: "Route",
    location,
    history,
    instantiateOpts: { userCaps: [["witchy-book"]] },
    slots: { "witchy-runnable": runnableSlot({ loadCompiler: async () => { throw new Error("no compiler in the render smoke test"); } }) },
  });
  await settle();

  // 3. The REAL book renders: a full nav from the real SUMMARY.md, and a real page.
  ok(fetchCalls.some((u) => u.includes("/content/SUMMARY.md")), "the app fetches the real SUMMARY.md");
  const navButtons = qsa(root, "nav").flatMap((n) => qsa(n, "button"));
  ok(navButtons.length >= 10, `the real SUMMARY.md renders a full nav (got ${navButtons.length} pages)`);
  ok(navButtons.some((b) => b.textContent === "Introduction"), "a real page title (Introduction) renders in the nav");
  ok(qsa(root, "h1").length + qsa(root, "h2").length >= 1, "the real home page renders a heading");
  ok(root.textContent.length > 200, "the real home page has substantial rendered content");

  // 4. A real book page's own `witchy` fences become editable runnable cells. Navigate to a page
  //    that carries witchy examples and check for a cell (textarea), trying a few candidate pages.
  const goto = (title) => {
    const b = qsa(root, "nav").flatMap((n) => qsa(n, "button")).find((x) => x.textContent === title);
    if (b) b.dispatchEvent({ type: "click" });
    return !!b;
  };
  let sawCell = qsa(root, "textarea").length > 0;
  for (const title of ["A Tour of the Language", "Getting Started", "Capabilities: The Heart of witchy", "Introduction"]) {
    if (sawCell) break;
    if (goto(title)) {
      await settle();
      sawCell = qsa(root, "textarea").length > 0 || qsa(root, "div").some((d) => (d.getAttribute("class") || "") === "witchy-cell");
    }
  }
  ok(sawCell, "a real book page's witchy fence became an editable runnable cell (a textarea)");
} finally {
  rmSync(dist, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-DOCS-BUNDLE FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-DOCS-BUNDLE OK");
process.exit(0);

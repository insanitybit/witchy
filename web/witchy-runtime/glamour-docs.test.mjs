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
import { runnableSlot } from "../witchy-runnable.js";
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
  set textContent(v) { this.childNodes = []; this.appendChild(new FakeText(v)); }
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
  // A cover link, three chapters (one with a NESTED sub-page), then a `---` rule and an appendix
  // — so the sidebar's depth/divider structure is exercised, not just a flat list.
  const SUMMARY = "# Summary\n\n[The witchy Book](title.md)\n\n- [Introduction](introduction.md)\n- [A Tour of the Language](tour.md)\n- [Capabilities](capabilities.md)\n  - [Narrowing](capabilities-narrowing.md)\n\n---\n\n[Appendix](appendix.md)\n";
  const calls = [];
  const fakeFetch = (url) => {
    calls.push(url);
    if (url.includes("/content/SUMMARY.md")) {
      return Promise.resolve({ status: 200, text: () => Promise.resolve(SUMMARY) });
    }
    const m = url.match(/\/content\/([^.]+)\.md/);
    const slug = m ? m[1] : "unknown";
    const title = slug.charAt(0).toUpperCase() + slug.slice(1);
    // Every page carries a runnable `witchy` example, so the docs app's slot-remap has
    // something to turn into a runnable cell (the `language-witchy` fence).
    const page = `## ${title}\n\nBody text for the **${slug}** page.\n\n\`\`\`witchy\nfn main(console: Console):\n    console.print("hi from ${slug}")\n\`\`\`\n`;
    return Promise.resolve({ status: 200, text: () => Promise.resolve(page) });
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
    // (RFC-0041 P2) register the runnable-cell renderer for the app's `witchy-runnable` slots.
    // `loadCompiler` is only called on Run (not driven here — the compile+run path is proven by
    // witchy-runnable.test.mjs), so it stays a stub; this asserts the SLOT WIRING renders a cell.
    slots: {
      "witchy-runnable": runnableSlot({
        loadCompiler: async () => { throw new Error("compiler not loaded in the rendering test"); },
      }),
    },
  });

  // 1. The sidebar is DERIVED from the fetched SUMMARY.md (not hardcoded).
  await settle();
  ok(calls.some((u) => u.includes("/content/SUMMARY.md")), "the app fetches SUMMARY.md for the nav");
  const navButtons = qsa(root, "nav").flatMap((n) => qsa(n, "button"));
  const clsOf = (n) => n.getAttribute("class") || "";
  // brand + 5 page buttons (Introduction, Tour, Capabilities, Narrowing, Appendix); the cover
  // (title.md) is NOT a list item — it's the header link — and the `---` is a non-button divider.
  ok(navButtons.length === 6, "the sidebar renders the brand + one button per page link");
  ok(navButtons.map((b) => b.textContent).includes("Capabilities"), "a page title parsed from SUMMARY.md renders");
  ok(navButtons.map((b) => b.textContent).includes("A Tour of the Language"), "a multi-word SUMMARY title parses correctly");
  // The sidebar header is the clickable cover link (not a duplicated <h1>).
  ok(navButtons.some((b) => clsOf(b).includes("sidebar-brand") && b.textContent === "The witchy Book"), "the book title renders as the cover link");
  ok(!qsa(root, "li").some((li) => li.textContent === "The witchy Book"), "the cover is NOT repeated as a list item");
  // The SUMMARY's two-level structure survives: the sub-page nests (depth-1), the `---` is a divider.
  ok(navButtons.some((b) => b.textContent === "Narrowing" && clsOf(b).includes("nav-depth-1")), "a sub-page renders nested at depth 1");
  ok(navButtons.some((b) => b.textContent === "Capabilities" && clsOf(b).includes("nav-depth-0")), "its parent chapter renders at depth 0");
  ok(qsa(root, "li").some((li) => clsOf(li).includes("nav-divider")), "the `---` rule renders an appendix divider");

  // 2. The initial route fetched the home page and rendered its Markdown to real elements.
  ok(calls.some((u) => u.includes("/content/introduction.md")), "the initial route fetches the home page");
  ok(qsa(root, "h2").some((h) => h.textContent === "Introduction"), "the fetched Markdown renders to a real <h2>");
  ok(root.textContent.includes("Body text for the"), "the page body renders");
  // (RFC-0041 P2) the page's `witchy` fence became a RUNNABLE CELL via a host Slot — the docs
  // app is now a runnable book. Crucially, the page ALSO still renders (the slot is non-diffed,
  // so — unlike the afterRender-mutation approach — a re-render doesn't corrupt the page).
  ok(qsa(root, "button").some((b) => (b.getAttribute("class") || "").includes("witchy-run")), "a code block became a runnable cell (Run button) via the host slot");
  ok(qsa(root, "div").some((d) => (d.getAttribute("class") || "") === "witchy-cell"), "the runnable cell is wrapped in the rendered page");

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

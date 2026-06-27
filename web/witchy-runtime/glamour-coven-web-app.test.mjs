#!/usr/bin/env node
// RFC-0015 Phase D: the COMPLETE coven-web frontend as one glamour app, end to end and
// headless. Mounts `coven_web_app` with an injected fake registry (fetch), history, and host
// ports (passkey register/login, promote), then drives the whole shell: catalog + capability
// search -> register -> sign in -> version record -> promote -> API docs -> inline source ->
// registry trust. Every fetch and the WebAuthn ceremony run in the host (the rune holds no
// Net and no token), proving the trusted shell that replaces the TypeScript app works with
// authority entirely at the edge.
//
// Usage:  node web/witchy-runtime/glamour-coven-web-app.test.mjs [path/to/witchy-binary]

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
const clickPrefix = (root, prefix) => {
  const b = qsa(root, "button").find((x) => x.textContent.startsWith(prefix));
  if (!b) throw new Error("no button starting with: " + prefix);
  b.dispatchEvent({ type: "click" });
};
const search = (root, value) => {
  const input = qsa(root, "input").find((x) => (x.getAttribute("class") || "").includes("search"));
  if (!input) throw new Error("no search input");
  input.dispatchEvent({ type: "input", target: { value } });
};

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

const work = mkdtempSync(join(tmpdir(), "glamour-cwa-"));
try {
  for (const f of ["src/glamour.witchy", "src/markdown.witchy"]) {
    copyFileSync(join(REPO, "projects/glamour", f), join(work, f.replace("src/", "")));
  }
  copyFileSync(join(REPO, "projects/glamour/examples/coven_web_app/src/coven_web_app.witchy"), join(work, "coven_web_app.witchy"));
  const wasmPath = join(work, "coven_web_app.wasm");
  execFileSync(BIN, ["compile", join(work, "coven_web_app.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  const calls = [];
  const json = (o) => Promise.resolve({ status: 200, text: () => Promise.resolve(JSON.stringify(o)) });
  const text = (s) => Promise.resolve({ status: 200, text: () => Promise.resolve(s) });
  const fakeFetch = (url) => {
    calls.push(url);
    if (url.includes("/catalog")) return json({ runes: [
      { name: "acme/charts", version: "1.0.0", state: "released", uploaded_by: "acme-bot", released_at: 1750000000, runtime_footprint: ["Net[Connect, Tls]", "Js{d3-runes-chart}"] },
      { name: "util/strings", version: "2.1.0", state: "staged", uploaded_by: "u-dev", released_at: 0, runtime_footprint: ["Console"] },
    ] });
    if (url.includes("/record")) return json({ state: "released", version: "1.0.0", released_at: 1750000000, determinism: "guaranteed", runtime_footprint: ["Net[Connect, Tls]"], hash: "sha256:abc", uploaded_by: "acme-bot", provenance: "trusted-publisher|issuer=gh" });
    if (url.includes("/doc")) return json({ markdown: "## acme/charts\n\nA charting library." });
    if (url.includes("/source")) return json({ files: [["witchy.toml", "[rune]\nname = \"acme/charts\""], ["src/charts.witchy", "// a chart rune\nfn draw():\n    pass"]] });
    if (url.includes("/rootpub")) return text("ed25519:3a9fROOTKEY");
    return text("");
  };
  const location = { pathname: "/" };
  const history = { pushState: (_s, _t, p) => { location.pathname = p; } };
  const ports = {
    passkeyRegister: () => Promise.resolve("passkey registered — now sign in"),
    passkeyLogin: () => Promise.resolve("alice"),
    promote: () => Promise.resolve("released ✓"),
  };

  const root = new FakeElement("root");
  await mount(wasm, root, {
    document: fakeDocument,
    initialModel: { route: "/", session: "", data: "", notice: "", query: "" },
    fetch: fakeFetch, routeTag: "Route", location, history, ports,
  });

  // 1. Catalog + capability search.
  await settle();
  ok(calls.some((u) => u.includes("/catalog")), "initial route fetches the catalog");
  ok(root.textContent.includes("acme/charts") && root.textContent.includes("util/strings"), "the catalog lists every rune");
  ok(root.textContent.includes("Js{d3-runes-chart}"), "a rune's foreign-code footprint badge renders");
  // crates.io-style metadata leads each row: author byline, lifecycle badge, publish date.
  ok(root.textContent.includes("by acme-bot"), "each row shows the author byline");
  ok(qsa(root, "span").some((s) => (s.getAttribute("class") || "").includes("badge state-released")), "the lifecycle state renders as a badge");
  ok(root.textContent.includes("published") && root.textContent.includes("not yet released"), "a released rune shows its publish date; a staged one says so");
  search(root, "strings");
  await settle();
  ok(!root.textContent.includes("acme/charts") && root.textContent.includes("util/strings"), "the search box filters by name");
  search(root, "Net");
  await settle();
  ok(root.textContent.includes("acme/charts") && !root.textContent.includes("util/strings"), "the search box ALSO filters by capability (Net)");
  search(root, "u-dev");
  await settle();
  ok(root.textContent.includes("util/strings") && !root.textContent.includes("acme/charts"), "the search box ALSO filters by author");
  search(root, "");
  await settle();
  ok(root.textContent.includes("acme/charts") && root.textContent.includes("util/strings"), "clearing the filter restores the full list");

  // 2. Register a passkey (host ceremony), then sign in.
  clickText(root, "register");
  await settle();
  ok(root.textContent.includes("passkey registered"), "the register port runs the WebAuthn create ceremony in the host");
  clickText(root, "sign in with passkey");
  await settle();
  ok(root.textContent.includes("signed in as alice"), "the passkey login port signs the user in (identity only — no token in the rune)");

  // 3. Open the version record (now signed in -> promote/yank visible).
  clickPrefix(root, "acme/charts");
  await settle();
  ok(calls.some((u) => u.includes("/record?name=acme~charts&version=1.0.0")), "clicking a rune fetches its record");
  ok(root.textContent.includes("trusted-publisher"), "the version view renders the provenance (security audit)");
  ok(qsa(root, "button").some((b) => b.textContent === "promote"), "promote/yank controls appear once signed in");

  // 4. Promote via the host 2FA port — the record re-fetches so state reflects the write.
  clickText(root, "promote");
  await settle();
  ok(root.textContent.includes("released ✓"), "the promote port runs and its result shows as a notice");
  ok(calls.filter((u) => u.includes("/record?name=acme~charts")).length >= 2, "a successful write re-fetches the record");

  // 5. Inline source viewer (untrusted source rendered as data — no sandbox). The "source →"
  // link lives on the version page, where the post-promote re-fetch has left us.
  clickText(root, "source →");
  await settle();
  ok(calls.some((u) => u.includes("/source?name=acme~charts")), "the source route fetches the package source");
  ok(root.textContent.includes("src/charts.witchy") && root.textContent.includes("fn draw():"), "every source file renders inline as <pre><code>");
  ok(qsa(root, "pre").length >= 2, "each file gets its own code block");
  clickPrefix(root, "← acme/charts");
  await settle();

  // 6. Generated API docs.
  clickText(root, "API documentation →");
  await settle();
  ok(calls.some((u) => u.includes("/doc?name=acme~charts")), "the docs route fetches the docs");
  ok(root.textContent.includes("A charting library") && qsa(root, "h2").length >= 1, "the docs Markdown renders to real elements");

  // 7. Registry trust.
  clickText(root, "Trust");
  await settle();
  ok(calls.some((u) => u.includes("/rootpub")), "the trust route fetches the TUF root key");
  ok(root.textContent.includes("Registry trust") && root.textContent.includes("ROOTKEY"), "the trust view renders the root key");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-COVEN-WEB-APP FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-COVEN-WEB-APP OK");
process.exit(0);

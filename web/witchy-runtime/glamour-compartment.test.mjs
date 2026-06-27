#!/usr/bin/env node
// RFC-0015 Phase B: prove the `compartment` primitive isolates foreign code. A rune
// embeds a `glamour.compartment("d3-runes-chart", grant, "ChartResized")` — the way you
// would drop in a third-party d3 chart. We assert the host shell renders it as a
// LOCKED-DOWN iframe (opaque origin via `sandbox="allow-scripts"` with NO
// `allow-same-origin`, loaded from the sealed `/compartments/<id>/` path) and NEVER
// inlines the foreign renderer or the grant data into the trusted page. The actual
// origin/CSP enforcement is the BROWSER's job; this verifies the configuration that
// makes "even a compromised d3 is contained" true — there is no code path that puts
// foreign content anywhere but inside that boxed frame.
//
// Usage:  node web/witchy-runtime/glamour-compartment.test.mjs [path/to/witchy-binary]

import { mount } from "./glamour-dom.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, copyFileSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");

// --- a tiny fake DOM (self-contained, matches the sibling drivers) -----------
class FakeNode {
  constructor() {
    this.childNodes = [];
    this.parentNode = null;
  }
  appendChild(child) {
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    this.childNodes.push(child);
    return child;
  }
  removeChild(child) {
    const i = this.childNodes.indexOf(child);
    if (i >= 0) this.childNodes.splice(i, 1);
    child.parentNode = null;
    return child;
  }
  replaceChild(next, prev) {
    const i = this.childNodes.indexOf(prev);
    if (i < 0) throw new Error("replaceChild: old node not found");
    this.childNodes[i] = next;
    next.parentNode = this;
    prev.parentNode = null;
    return prev;
  }
}
class FakeText extends FakeNode {
  constructor(text) {
    super();
    this._text = text;
  }
  get textContent() {
    return this._text;
  }
  set textContent(v) {
    this._text = v;
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
  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }
  getAttribute(name) {
    return this.attributes.has(name) ? this.attributes.get(name) : null;
  }
  removeAttribute(name) {
    this.attributes.delete(name);
  }
  addEventListener(event, fn) {
    if (!this.listeners.has(event)) this.listeners.set(event, new Set());
    this.listeners.get(event).add(fn);
  }
  removeEventListener(event, fn) {
    const s = this.listeners.get(event);
    if (s) s.delete(fn);
  }
  get textContent() {
    let out = "";
    for (const c of this.childNodes) out += c.textContent;
    return out;
  }
  allAttrValues() {
    return [...this.attributes.values()];
  }
}
const fakeDocument = {
  createElement: (tag) => new FakeElement(tag),
  createTextNode: (text) => new FakeText(text),
};
function querySelectorAll(node, tag, acc = []) {
  if (node instanceof FakeElement && node.el === tag) acc.push(node);
  for (const c of node.childNodes) if (c instanceof FakeElement) querySelectorAll(c, tag, acc);
  return acc;
}
function allElements(node, acc = []) {
  if (node instanceof FakeElement) acc.push(node);
  for (const c of node.childNodes) if (c instanceof FakeElement) allElements(c, acc);
  return acc;
}

// A distinctive opaque grant token (the host passes `grant` through verbatim; the test
// only needs to prove it never lands in the trusted DOM, so a sentinel beats real JSON).
const GRANT = "GRANTSENTINEL42";
const RUNE = `
import glamour
import json
import reflect

type Msg derive(Reflect):
    ChartResized(Int)

fn view(model: Int) -> VNode(Msg):
    glamour.element("section", [], [
        glamour.element("h1", [], [glamour.text("Runes published over time")]),
        glamour.compartment("d3-runes-chart", "${GRANT}", "ChartResized"),
    ])

fn update(model: Int, msg: Msg) -> (Int, Cmd(Msg)):
    (model, NoCmd)

fn parse_model(j: Json) -> Int:
    match json.as_int(j):
        Some(n) -> n
        None -> 0

fn parse_msg(j: Json) -> Msg:
    ChartResized(0)

fn model_to_json(model: Int) -> Json:
    json.value_of(model)

fn msg_to_json(m: Msg) -> Json:
    json.value_of(m)

pub fn export_step(input: String) -> String:
    step_with(input, view, update, parse_model, parse_msg, model_to_json, msg_to_json)

fn main(console: Console):
    print(console, export_step("{\\"model\\": 0}") + "\\n")
`;

let failures = 0;
const ok = (cond, msg) => {
  console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`);
  if (!cond) failures++;
};

const work = mkdtempSync(join(tmpdir(), "glamour-comp-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  writeFileSync(join(work, "chart.witchy"), RUNE);
  const wasmPath = join(work, "chart.wasm");
  execFileSync(BIN, ["compile", join(work, "chart.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  const root = new FakeElement("root");
  await mount(wasm, root, { document: fakeDocument, initialModel: 0 });

  // The foreign renderer is a SINGLE locked-down iframe — never inline content.
  const frames = querySelectorAll(root, "iframe");
  ok(frames.length === 1, "the compartment renders exactly one <iframe>");
  const frame = frames[0];
  // Opaque origin: scripts may run, but NO allow-same-origin -> no cookies/parent/storage.
  ok(frame.getAttribute("sandbox") === "allow-scripts", "the iframe is sandboxed to an opaque origin (allow-scripts, no allow-same-origin)");
  ok(
    frame.getAttribute("src") === "/compartments/d3-runes-chart/",
    "the iframe loads the sealed renderer path (its own connect-src 'none' CSP)",
  );

  // The grant (public chart data) is delivered over the channel, NEVER inlined into the
  // trusted DOM — and nothing foreign is rendered into the parent.
  const inlinedGrant = allElements(root).some((el) => el.allAttrValues().some((v) => v.includes(GRANT)));
  ok(!inlinedGrant, "the grant data is not inlined into any trusted-DOM attribute");
  ok(!root.textContent.includes(GRANT), "no foreign/grant content leaks into the trusted page text");
  // The trusted page itself still rendered (the heading is real, in the parent).
  ok(querySelectorAll(root, "h1").length === 1, "the trusted shell around the compartment renders normally");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-COMPARTMENT FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-COMPARTMENT OK");
// A compartment opens a MessageChannel (an open port keeps the event loop alive); exit
// explicitly now that all checks have passed.
process.exit(0);

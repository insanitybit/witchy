#!/usr/bin/env node
// RFC-0015 Phase C: prove keyed list reconciliation. A rune renders a `<ul>` of
// `keyed(k, <li>)` children, then reorders them. We mark each live `<li>` node before the
// reorder and assert that after it, the SAME node instances are present in the new order —
// i.e. the host MOVED the existing nodes (preserving identity, and thus focus/caret) rather
// than rebuilding the list by position. This is what keeps a focused input alive when a
// catalog/search list reorders.
//
// Usage:  node web/witchy-runtime/glamour-keyed.test.mjs [path/to/witchy-binary]

import { mount } from "./glamour-dom.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, copyFileSync, rmSync, readFileSync } from "node:fs";
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

const RUNE = `
import glamour
import json
import reflect

type Msg derive(Reflect):
    Reorder

fn order(model: Int) -> List(String):
    if model == 0:
        ["a", "b", "c"]
    else:
        ["c", "a", "b"]

fn view(model: Int) -> VNode(Msg):
    var lis = []
    for k in order(model):
        lis = list.push(lis, glamour.keyed(k, glamour.element("li", [glamour.prop("data-key", k)], [glamour.text(k)])))
    glamour.element("div", [], [
        glamour.element("button", [glamour.on("click", Reorder)], [glamour.text("reorder")]),
        glamour.element("ul", [], lis),
    ])

fn update(model: Int, msg: Msg) -> (Int, Cmd(Msg)):
    (1 - model, NoCmd)

fn parse_model(j: Json) -> Int:
    match json.as_int(j):
        Some(n) -> n
        None -> 0

fn parse_msg(j: Json) -> Msg:
    Reorder

fn model_to_json(model: Int) -> Json:
    json.value_of(model)

fn msg_to_json(m: Msg) -> Json:
    json.value_of(m)

pub fn export_step(input: String) -> String:
    step_with(input, view, update, parse_model, parse_msg, model_to_json, msg_to_json)

fn main(console: Console):
    print(console, export_step("{\\"model\\": 0}"))
`;

let failures = 0;
const ok = (cond, msg) => {
  console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`);
  if (!cond) failures++;
};

const work = mkdtempSync(join(tmpdir(), "glamour-keyed-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  writeFileSync(join(work, "klist.witchy"), RUNE);
  const wasmPath = join(work, "klist.wasm");
  execFileSync(BIN, ["compile", join(work, "klist.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  const root = new FakeElement("root");
  await mount(wasm, root, { document: fakeDocument, initialModel: 0 });

  const before = querySelectorAll(root, "li");
  ok(before.map((n) => n.getAttribute("data-key")).join(",") === "a,b,c", "initial list renders in order a,b,c");
  // Mark each live node so we can detect reuse vs rebuild after the reorder.
  before.forEach((n) => (n.__mark = n.getAttribute("data-key")));

  // Reorder to c,a,b.
  querySelector(root, "button").dispatchEvent({ type: "click" });

  const after = querySelectorAll(root, "li");
  ok(after.map((n) => n.getAttribute("data-key")).join(",") === "c,a,b", "the list reorders to c,a,b");
  // Identity preserved: every reordered node is the SAME instance (its mark survived),
  // i.e. it was MOVED, not recreated — keyed reconciliation, not index rebuild.
  ok(
    after.every((n) => n.__mark === n.getAttribute("data-key")),
    "each reordered <li> is the same DOM node moved into place (focus/caret would survive)",
  );
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-KEYED FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-KEYED OK");

#!/usr/bin/env node
// RFC-0041: the host SLOT — a subtree the host mounts and glamour NEVER diffs into. A rune
// emits `glamour.slot("demo", data)` alongside an ordinary counter; the host registers a
// `demo` renderer. We assert the slot renders via that renderer, and — the whole point —
// that after a re-render (the counter bumps) the host's widget node is the SAME instance
// (a host mutation to it survives) and the renderer was NOT called again. That is exactly
// what a runnable code cell needs: glamour re-renders the page on every message, and a cell
// mounted in a slot must not be clobbered. This is the fix for the P2 wiring finding.
//
// Usage:  node web/witchy-runtime/glamour-slot.test.mjs [path/to/witchy-binary]

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
}
const fakeDocument = { createElement: (t) => new FakeElement(t), createTextNode: (t) => new FakeText(t) };
const qsa = (node, tag, acc = []) => {
  if (node instanceof FakeElement && node.el === tag) acc.push(node);
  for (const c of node.childNodes) if (c instanceof FakeElement) qsa(c, tag, acc);
  return acc;
};

const RUNE = `
import glamour
from glamour import VNode, Attr, HAttr, HtmlTok, Cmd, UiRoot, UiFetch, UiRoute, UiTimer, SecretInput, SecretRef, CredentialPort
import json
from json import Json
import reflect

type Msg derive(Reflect):
    Bump

fn view(n: Int) -> VNode(Msg):
    glamour.element("div", [], [
        glamour.element("button", [glamour.on("click", Bump)], [glamour.text("bump")]),
        glamour.element("span", [glamour.prop("class", "count")], [glamour.text("\${n}")]),
        glamour.slot("demo", "the-payload"),
    ])

fn update(n: Int, msg: Msg) -> (Int, Cmd(Msg)):
    match msg:
        Bump -> (n + 1, NoCmd)

fn parse_model(j: Json) -> Int:
    json.as_int(j).unwrap_or(0)

fn parse_msg(j: Json) -> Msg:
    Bump

fn model_to_json(n: Int) -> Json:
    json.from_value(n)

fn msg_to_json(m: Msg) -> Json:
    json.from_value(m)

pub fn export_step(input: String) -> String:
    glamour.step_with(input, view, update, parse_model, parse_msg, model_to_json, msg_to_json)

fn main(console: Console):
    console.print(export_step("{\\"model\\": 0}"))
`;

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

const work = mkdtempSync(join(tmpdir(), "glamour-slot-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  writeFileSync(join(work, "slotdemo.witchy"), RUNE);
  const wasmPath = join(work, "slotdemo.wasm");
  execFileSync(BIN, ["compile", join(work, "slotdemo.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  // The host's `demo` slot renderer: build a widget node, count calls.
  const slotCalls = [];
  const slots = {
    demo: (doc, data) => {
      slotCalls.push(data);
      const el = doc.createElement("div");
      el.setAttribute("class", "hostwidget");
      el.setAttribute("data-payload", data);
      return el;
    },
  };

  const root = new FakeElement("root");
  const app = await mount(wasm, root, { document: fakeDocument, initialModel: 0, slots });

  // The slot rendered via the registered renderer, with the rune's payload.
  const widgets = qsa(root, "div").filter((d) => d.getAttribute("class") === "hostwidget");
  ok(widgets.length === 1, "the slot rendered via the registered `demo` renderer");
  ok(widgets[0].getAttribute("data-payload") === "the-payload", "the renderer received the rune's slot data");
  ok(slotCalls.length === 1, "the renderer was called exactly once");
  ok(qsa(root, "span").some((s) => s.textContent === "0"), "the ordinary counter rendered (0)");

  // Simulate host state the framework must not clobber: mutate the widget.
  const widget = widgets[0];
  widget.__hostState = "alive";

  // Re-render by bumping the counter (an ordinary msg — the page re-renders).
  qsa(root, "button")[0].dispatchEvent({ type: "click" });
  ok(app.getModel() === 1 && qsa(root, "span").some((s) => s.textContent === "1"), "the counter re-rendered to 1 (a real re-render happened)");

  // The whole point: glamour did NOT diff into the slot. Same node, host state intact,
  // renderer not called again.
  const after = qsa(root, "div").filter((d) => d.getAttribute("class") === "hostwidget");
  ok(after.length === 1 && after[0] === widget, "the host widget is the SAME node after a re-render (not re-created)");
  ok(after[0].__hostState === "alive", "the host's mutation to the widget survived the re-render");
  ok(slotCalls.length === 1, "the slot renderer was NOT called again on re-render (glamour never diffs into the slot)");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-SLOT FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-SLOT OK");
process.exit(0);

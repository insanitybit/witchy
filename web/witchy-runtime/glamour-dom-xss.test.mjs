#!/usr/bin/env node
// RFC-0015 Phase A: prove glamour's DOM shell is XSS-safe at the attribute layer.
// A rune's `view` emits deliberately hostile attributes — a `javascript:` href, a
// `javascript:` img src, and a string `onclick` handler — and we assert the shell
// NEUTRALIZES them when building the DOM: URL attributes are scheme-checked (hostile
// schemes collapse to `#`), and `on*` attributes are never written (handlers attach
// ONLY via the typed `on(event, msg)` path). Safe URLs (relative, https) pass through.
//
// This exercises the real render path (mount -> createNode -> applyAttr), the same
// single choke point both create and update route through. Usage:
//   node web/witchy-runtime/glamour-dom-xss.test.mjs [path/to/witchy-binary]

import { mount } from "./glamour-dom.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, copyFileSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");

// --- a tiny fake DOM (self-contained, zero-dep — matches the sibling drivers) ----
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

// --- the hostile rune: its view emits attributes an attacker would inject --------
const RUNE = `
import glamour
import json
import reflect

type Msg derive(Reflect):
    Noop

fn view(model: Int) -> VNode(Msg):
    let evil = glamour.element("a", [glamour.prop("href", "javascript:alert(1)"), glamour.prop("onclick", "steal()"), glamour.prop("class", "danger")], [glamour.text("evil")])
    let rel = glamour.element("a", [glamour.prop("href", "/page")], [glamour.text("rel")])
    let ok = glamour.element("a", [glamour.prop("href", "https://example.com")], [glamour.text("abs")])
    let pic = glamour.element("img", [glamour.prop("src", "javascript:alert(2)")], [])
    glamour.element("div", [], [evil, rel, ok, pic])

fn update(model: Int, msg: Msg) -> (Int, Cmd(Msg)):
    (model, NoCmd)

fn parse_model(j: Json) -> Int:
    match json.as_int(j):
        Some(n) -> n
        None -> 0

fn parse_msg(j: Json) -> Msg:
    Noop

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

const work = mkdtempSync(join(tmpdir(), "glamour-xss-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  writeFileSync(join(work, "xss.witchy"), RUNE);
  const wasmPath = join(work, "xss.wasm");
  execFileSync(BIN, ["compile", join(work, "xss.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  const root = new FakeElement("root");
  await mount(wasm, root, { document: fakeDocument, initialModel: 0 });

  const links = querySelectorAll(root, "a");
  ok(links.length === 3, "three <a> elements render");
  const [evil, rel, abs] = links;

  // The hostile link: javascript: href neutralized, string handler dropped.
  ok(evil.getAttribute("href") === "#", "javascript: href is sanitized to #");
  ok(evil.getAttribute("onclick") === null, "string onclick handler is never written");
  ok(evil.getAttribute("class") === "danger", "a benign attribute on the same element survives");

  // Safe URLs pass through untouched.
  ok(rel.getAttribute("href") === "/page", "a relative href passes through");
  ok(abs.getAttribute("href") === "https://example.com", "an https href passes through");

  // The hostile image source is neutralized too.
  const imgs = querySelectorAll(root, "img");
  ok(imgs.length === 1 && imgs[0].getAttribute("src") === "#", "javascript: img src is sanitized to #");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-DOM XSS FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-DOM XSS OK");

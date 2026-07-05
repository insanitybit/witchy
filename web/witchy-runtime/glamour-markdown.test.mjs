#!/usr/bin/env node
// RFC-0015 Phase A3: prove the Markdown renderer is XSS-safe. A rune renders
// deliberately hostile UNTRUSTED Markdown — a raw `<script>` tag, a `javascript:`
// link — via `markdown.to_vnode`, and we assert the resulting DOM is inert: raw HTML
// is shown as literal TEXT (no `<script>` element is ever created — glamour has no
// HTML-string sink), and the link's `javascript:` href is neutralized to `#`. Normal
// Markdown (heading, bold, code, list) still renders to real elements.
//
// Usage:  node web/witchy-runtime/glamour-markdown.test.mjs [path/to/witchy-binary]

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

// --- the rune: renders hostile untrusted Markdown via markdown.to_vnode ------
const RUNE = `
import glamour
from glamour import VNode, Attr, HAttr, HtmlTok, Cmd, UiRoot, UiFetch, UiRoute, UiTimer, SecretInput, SecretRef, CredentialPort
import markdown
import json
from json import Json
import reflect

type Msg derive(Reflect):
    Noop

fn view(model: Int) -> VNode(Msg):
    markdown.to_vnode("# Heading\\n\\nA raw <script>alert(1)</script> tag and a [trap](javascript:steal()) link and **bold** text.\\n\\n- item one\\n- item two\\n\\n| Left | Right |\\n|---|---|\\n| one | two |\\n| three | four |\\n")

fn update(model: Int, msg: Msg) -> (Int, Cmd(Msg)):
    (model, NoCmd)

fn parse_model(j: Json) -> Int:
    match json.as_int(j):
        Some(n) -> n
        None -> 0

fn parse_msg(j: Json) -> Msg:
    Noop

fn model_to_json(model: Int) -> Json:
    json.from_value(model)

fn msg_to_json(m: Msg) -> Json:
    json.from_value(m)

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

const work = mkdtempSync(join(tmpdir(), "glamour-md-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  copyFileSync(join(REPO, "projects/glamour/src/markdown.witchy"), join(work, "markdown.witchy"));
  writeFileSync(join(work, "mdview.witchy"), RUNE);
  const wasmPath = join(work, "mdview.wasm");
  execFileSync(BIN, ["compile", join(work, "mdview.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  const root = new FakeElement("root");
  await mount(wasm, root, { document: fakeDocument, initialModel: 0 });

  // The headline safety property: the raw <script> NEVER becomes a <script> element.
  ok(querySelectorAll(root, "script").length === 0, "a raw <script> in the Markdown creates NO <script> element");
  // It is shown as literal text instead (glamour escapes by construction).
  ok(root.textContent.includes("<script>alert(1)</script>"), "the raw <script> is rendered as visible text");

  // The javascript: link href is neutralized to #.
  const links = querySelectorAll(root, "a");
  ok(links.length === 1 && links[0].getAttribute("href") === "#", "a javascript: link href is sanitized to #");

  // Normal Markdown still renders to real elements.
  ok(querySelectorAll(root, "h1").length === 1, "a heading renders as <h1>");
  ok(querySelectorAll(root, "strong").length === 1, "**bold** renders as <strong>");
  ok(querySelectorAll(root, "li").length === 2, "a list renders two <li>");
  // A GFM table renders as a real <table> (header <th> + body <td>), not literal pipe text.
  ok(querySelectorAll(root, "table").length === 1, "a GFM table renders one <table>");
  ok(querySelectorAll(root, "th").length === 2, "the header row renders two <th>");
  ok(querySelectorAll(root, "td").length === 4, "the two body rows render four <td>");
  ok(!root.textContent.includes("| Left | Right |"), "no literal pipe row leaks into the text");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-MARKDOWN FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-MARKDOWN OK");

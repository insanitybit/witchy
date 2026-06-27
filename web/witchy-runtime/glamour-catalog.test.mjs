#!/usr/bin/env node
// RFC-0015 Phase D: the coven catalog/index view on glamour. Mounts the `catalog` example
// and drives its live SEARCH box — proving `on_input` carries the field's value into the
// MVU loop as data (the rune holds no DOM), and that the KEYED rune cards filter by name AND
// by capability. This is the registry home page, built on the framework.
//
// Usage:  node web/witchy-runtime/glamour-catalog.test.mjs [path/to/witchy-binary]

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
const names = (root) => querySelectorAll(root, "button").map((b) => b.textContent);

let failures = 0;
const ok = (cond, msg) => {
  console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`);
  if (!cond) failures++;
};
// Type `value` into the search box (fire an `input` event the on_input handler reads).
const type = (root, value) =>
  querySelector(root, "input").dispatchEvent({ type: "input", target: { value } });

const work = mkdtempSync(join(tmpdir(), "glamour-catalog-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  copyFileSync(join(REPO, "projects/glamour/examples/catalog/src/catalog.witchy"), join(work, "catalog.witchy"));
  const wasmPath = join(work, "catalog.wasm");
  execFileSync(BIN, ["compile", join(work, "catalog.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  const root = new FakeElement("root");
  await mount(wasm, root, { document: fakeDocument, initialModel: "" });

  // The full catalog renders, with a search box and footprint badges.
  ok(querySelectorAll(root, "li").length === 3, "the full catalog renders three runes");
  ok(querySelector(root, "input") !== null, "a search box renders");
  ok(
    querySelectorAll(root, "span").some((s) => s.textContent === "Js{d3-runes-chart}"),
    "a rune's foreign-code footprint badge renders",
  );

  // Search by NAME: typing `json` narrows to std/json.
  type(root, "json");
  ok(querySelectorAll(root, "li").length === 1 && names(root).includes("std/json"), "search by name filters the list");

  // Clearing restores the full catalog (keyed cards reappear).
  type(root, "");
  ok(querySelectorAll(root, "li").length === 3, "clearing the search restores the full catalog");

  // Search by CAPABILITY: typing `Js` matches the rune whose footprint includes a compartment.
  type(root, "Js");
  ok(
    querySelectorAll(root, "li").length === 1 && names(root).includes("acme/charts"),
    "search by capability filters to the matching rune",
  );
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-CATALOG FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-CATALOG OK");

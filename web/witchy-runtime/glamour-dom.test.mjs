#!/usr/bin/env node
// RFC-0008 capstone test: drive the FULL glamour MVU stack headlessly — compile
// the counter rune to WASM, mount it through the DOM host shell (glamour-dom.mjs)
// into a fake DOM, assert the initial render, simulate a `+` click, and assert the
// span re-renders with the incremented count. This proves render + event ->
// update -> re-render end to end, without a browser.
//
// Usage:  node web/witchy-runtime/glamour-dom.test.mjs [path/to/witchy-binary]
// Exit 0 on success; non-zero (with a message) on any failure.
//
// A minimal, self-contained fake DOM stands in for jsdom — enough of the surface
// the shell uses (createElement / createTextNode / textContent / setAttribute /
// addEventListener / appendChild / replaceChild / removeChild / childNodes /
// dispatchEvent). This keeps the test ZERO-dependency, matching coven-web's
// zero-dep posture; the shell touches only these standard DOM APIs, so a fake that
// implements them faithfully is a sound stand-in (and a real browser/jsdom would
// run the exact same shell unchanged).

import { mount } from "./glamour-dom.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, copyFileSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");

// --- a tiny fake DOM --------------------------------------------------------
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
    if (next.parentNode) next.parentNode.removeChild(next);
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
    this.tagName = tag.toUpperCase();
    this.el = tag;
    this.attributes = new Map();
    this.listeners = new Map(); // event -> Set<fn>
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
  dispatchEvent(event) {
    const s = this.listeners.get(event.type);
    if (s) for (const fn of [...s]) fn(event);
    return true;
  }
  // textContent of an element is the concatenation of its descendants' text
  // (read-only helper for assertions).
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

// Find the first descendant element with the given tag.
function querySelector(node, tag) {
  if (node instanceof FakeElement && node.el === tag) return node;
  for (const c of node.childNodes) {
    if (c instanceof FakeElement || c instanceof FakeText) {
      const found = querySelector(c, tag);
      if (found) return found;
    }
  }
  return null;
}

// All descendant elements with the given tag (document order).
function querySelectorAll(node, tag, acc = []) {
  if (node instanceof FakeElement && node.el === tag) acc.push(node);
  for (const c of node.childNodes) {
    if (c instanceof FakeElement) querySelectorAll(c, tag, acc);
  }
  return acc;
}

// --- the test ---------------------------------------------------------------
let failures = 0;
const ok = (cond, msg) => {
  console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`);
  if (!cond) failures++;
};

const work = mkdtempSync(join(tmpdir(), "glamour-dom-"));
try {
  // Compile the counter rune (with glamour as a sibling so imports resolve) in an
  // isolated dir, via the binary under test.
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  copyFileSync(
    join(REPO, "projects/glamour/examples/counter/src/counter.witchy"),
    join(work, "counter.witchy"),
  );
  const wasmPath = join(work, "counter.wasm");
  execFileSync(BIN, ["compile", join(work, "counter.witchy"), "--out", wasmPath], {
    cwd: work,
  });
  const wasm = readFileSync(wasmPath);

  // Mount into the fake DOM with an initial model of count 0.
  const root = new FakeElement("root");
  const app = await mount(wasm, root, {
    document: fakeDocument,
    initialModel: 0, // Model = Int
  });

  // --- initial render ---
  const div = querySelector(root, "div");
  ok(div !== null, "the rune renders a <div> root");
  const span = querySelector(root, "span");
  ok(span !== null && span.textContent === "0", "the <span> shows the initial count 0");
  const buttons = querySelectorAll(root, "button");
  ok(buttons.length === 2, "two <button> elements render (- and +)");
  ok(
    buttons.some((b) => b.textContent === "+") && buttons.some((b) => b.textContent === "-"),
    "the buttons are labelled - and +",
  );
  // The + button carries a click handler (an `on` attr wired as addEventListener).
  const plus = buttons.find((b) => b.textContent === "+");
  ok(plus.listeners.get("click")?.size === 1, "the + button has a click handler");

  // --- simulate a + click: the handler dispatches the Inc msg, the rune folds it
  //     into count+1, and the span re-renders ---
  plus.dispatchEvent({ type: "click" });
  const span1 = querySelector(root, "span");
  ok(span1.textContent === "1", "after a + click the <span> updates to 1");
  ok(app.getModel() === 1, "the model advanced to 1");

  // --- a second + click compounds (the loop is stable across renders) ---
  const plus2 = querySelectorAll(root, "button").find((b) => b.textContent === "+");
  plus2.dispatchEvent({ type: "click" });
  ok(querySelector(root, "span").textContent === "2", "a second + click yields 2");

  // --- a - click decrements (the other handler routes its own Inc/Dec msg) ---
  const minus = querySelectorAll(root, "button").find((b) => b.textContent === "-");
  minus.dispatchEvent({ type: "click" });
  ok(querySelector(root, "span").textContent === "1", "a - click decrements back to 1");

  // The differ patches in place: the same <div> node persists across re-renders
  // (no wholesale root replacement for a same-shaped tree).
  ok(querySelector(root, "div") === div, "the differ patches the existing DOM in place");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-DOM TEST FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-DOM OK");

#!/usr/bin/env node
// RFC-0008 EFFECTS-AS-DATA capstone test: prove the rune -> Cmd -> shell-performs
// -> msg -> update loop end to end, HEADLESSLY, with a FAKE CLOCK.
//
// The `autocounter` rune is empty-root-footprint: it CANNOT read a clock or arm a timer
// (it holds no `Clock`). Its `update` only DESCRIBES the next tick by returning a
// `Cmd` — `After(1000, Tick)`. The capability-HOLDING host shell
// (glamour-dom.mjs) reads that description and performs it via an INJECTED timer.
// Here that timer is a controllable fake queue: nothing fires until the test
// advances the clock, so the test deterministically drives "1 second passes" and
// asserts the model/DOM advanced WITHOUT any user click — proving the timer
// authority is the shell's, exercised purely from the rune's data description.
//
// Usage:  node web/witchy-runtime/glamour-dom-timer.test.mjs [path/to/witchy-binary]
// Exit 0 on success; non-zero (with a message) on any failure.
//
// Zero-dependency: a tiny self-contained fake DOM stands in for jsdom (the same
// shape the sibling glamour-dom.test.mjs uses), plus a fake timer the shell drives
// through `opts.setTimeout`.

import { mount } from "./glamour-dom.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, copyFileSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");

// --- a tiny fake DOM (same surface the shell uses) --------------------------
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
  dispatchEvent(event) {
    const s = this.listeners.get(event.type);
    if (s) for (const fn of [...s]) fn(event);
    return true;
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

function querySelectorAll(node, tag, acc = []) {
  if (node instanceof FakeElement && node.el === tag) acc.push(node);
  for (const c of node.childNodes) {
    if (c instanceof FakeElement) querySelectorAll(c, tag, acc);
  }
  return acc;
}

// --- a fake, controllable clock ---------------------------------------------
// `setTimeout(fn, ms)` enqueues a pending timer; nothing runs until the test calls
// `advance(ms)`, which fires every timer whose deadline has elapsed (re-armed
// timers scheduled DURING a fire run on the next `advance`, matching real timer
// semantics — a 1000ms tick re-arming a 1000ms tick won't infinite-loop here).
class FakeClock {
  constructor() {
    this.now = 0;
    this.pending = []; // { at, fn }
    this.nextId = 1;
  }
  setTimeout = (fn, ms) => {
    const id = this.nextId++;
    this.pending.push({ id, at: this.now + ms, fn });
    return id;
  };
  // Advance time by `ms`; fire all timers due at or before the new `now`. Timers
  // armed during this advance (at a future `at`) are left for a later advance.
  advance(ms) {
    const target = this.now + ms;
    // Loop because a fired timer can arm another that is ALSO due within this
    // window (e.g. an after(0)); guard against runaway with a step cap.
    let guard = 0;
    while (true) {
      const due = this.pending.filter((t) => t.at <= target).sort((a, b) => a.at - b.at);
      if (due.length === 0) break;
      const next = due[0];
      this.pending = this.pending.filter((t) => t !== next);
      this.now = next.at;
      next.fn();
      if (++guard > 100000) throw new Error("FakeClock.advance: runaway timer loop");
    }
    this.now = target;
  }
  pendingCount() {
    return this.pending.length;
  }
}

// --- the test ---------------------------------------------------------------
let failures = 0;
const ok = (cond, msg) => {
  console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`);
  if (!cond) failures++;
};

const work = mkdtempSync(join(tmpdir(), "glamour-timer-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  copyFileSync(
    join(REPO, "projects/glamour/examples/autocounter/src/autocounter.witchy"),
    join(work, "autocounter.witchy"),
  );
  const wasmPath = join(work, "autocounter.wasm");
  execFileSync(BIN, ["compile", join(work, "autocounter.witchy"), "--out", wasmPath], {
    cwd: work,
  });
  const wasm = readFileSync(wasmPath);

  // Mount with the FAKE clock injected as the shell's timer authority.
  const clock = new FakeClock();
  const root = new FakeElement("root");
  const app = await mount(wasm, root, {
    document: fakeDocument,
    initialModel: 0, // Model = Int
    setTimeout: clock.setTimeout,
    // (RFC-0039/0040) autocounter's `export_step` takes a `UiRoot`; stage its grant so
    // Glamour can narrow the `UiTimer` token that arms the self-sustaining tick.
    instantiateOpts: { userCaps: [["timer"]] },
  });

  // --- initial render: count 0, NO timer armed yet (initial cmd is `none`) ---
  const span = querySelector(root, "span");
  ok(span !== null && span.textContent === "0", "the <span> shows the initial count 0");
  ok(clock.pendingCount() === 0, "no timer is armed before the loop starts (initial cmd is `none`)");
  ok(app.getModel() === 0, "the model starts at 0");

  // --- kick the loop off with a `start` click. update returns After(1000,Tick),
  //     so the SHELL arms one timer (the rune only described it) ---
  const startBtn = querySelectorAll(root, "button").find((b) => b.textContent === "start");
  ok(startBtn != null, "the start button renders");
  startBtn.dispatchEvent({ type: "click" });
  ok(querySelector(root, "span").textContent === "1", "after start the count is 1");
  ok(clock.pendingCount() === 1, "the shell armed exactly one timer from the After Cmd");

  // --- advance the fake clock 1s: the timer fires, the SHELL dispatches the
  //     deferred `Tick` msg, the rune folds it into count+1 and re-arms the next
  //     After — all WITHOUT any user interaction. This is the rune->Cmd->shell->
  //     msg->update loop, proven by the model/DOM advancing on a clock tick. ---
  clock.advance(1000);
  ok(querySelector(root, "span").textContent === "2", "after 1s the timer auto-incremented to 2 (no click)");
  ok(app.getModel() === 2, "the model advanced to 2 via the timer-dispatched Tick");
  ok(clock.pendingCount() === 1, "the loop re-armed the next timer (a self-sustaining clock)");

  // --- two more seconds compound, proving the loop is stable across ticks ---
  clock.advance(1000);
  ok(querySelector(root, "span").textContent === "3", "after another 1s the count is 3");
  clock.advance(1000);
  ok(querySelector(root, "span").textContent === "4", "after another 1s the count is 4");
  ok(app.getModel() === 4, "the model is 4 after three timer ticks");

  // --- a PARTIAL advance does NOT fire the pending timer (proves the delay is
  //     honored: the shell holds a real ms deadline, not a fire-on-advance hack) ---
  clock.advance(500);
  ok(querySelector(root, "span").textContent === "4", "a 0.5s advance does not fire the 1s timer (delay honored)");
  clock.advance(500);
  ok(querySelector(root, "span").textContent === "5", "the remaining 0.5s fires it -> 5");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-DOM TIMER TEST FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-DOM TIMER OK");

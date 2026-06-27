#!/usr/bin/env node
// RFC-0015 Phase C: prove client-side routing. A rune maps `model` (the current path) to
// a view and returns `navigate(path)` to change it — it holds no history authority, only
// describes the navigation. The shell performs `history.pushState`, mirrors the new path
// back as the route msg, and re-delivers the route on Back/Forward (popstate). All
// injectable (history/location/popstate) so this runs headlessly. Asserts: the initial
// path is delivered; a `Nav` pushes history AND updates the view; a popstate (back) event
// re-delivers the route.
//
// Usage:  node web/witchy-runtime/glamour-routing.test.mjs [path/to/witchy-binary]

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

const RUNE = `
import glamour
import json
import reflect

type Msg derive(Reflect):
    Route(String)
    Go(String)

fn view(model: String) -> VNode(Msg):
    glamour.element("div", [], [
        glamour.element("span", [], [glamour.text(model)]),
        glamour.element("button", [glamour.on("click", Go("/about"))], [glamour.text("about")]),
    ])

fn update(model: String, msg: Msg) -> (String, Cmd(Msg)):
    match msg:
        Route(path) -> (path, NoCmd)
        Go(path) -> (model, glamour.navigate(path))

fn parse_model(j: Json) -> String:
    json.as_string(j).unwrap_or("/")

fn arg_str(j: Json, i: Int) -> String:
    match json.get(j, "$values"):
        Some(arr) ->
            match json.index(arr, i):
                Some(v) -> json.as_string(v).unwrap_or("")
                None -> ""
        None -> ""

fn parse_msg(j: Json) -> Msg:
    match json.get_string(j, "$variant"):
        Some(v) ->
            if v == "Go":
                Go(arg_str(j, 0))
            else:
                Route(arg_str(j, 0))
        None -> Route("/")

fn model_to_json(model: String) -> Json:
    json.value_of(model)

fn msg_to_json(m: Msg) -> Json:
    json.value_of(m)

pub fn export_step(input: String) -> String:
    step_with(input, view, update, parse_model, parse_msg, model_to_json, msg_to_json)

fn main(console: Console):
    print(console, export_step("{\\"model\\": \\"/\\"}"))
`;

let failures = 0;
const ok = (cond, msg) => {
  console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`);
  if (!cond) failures++;
};

const work = mkdtempSync(join(tmpdir(), "glamour-routing-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  writeFileSync(join(work, "router.witchy"), RUNE);
  const wasmPath = join(work, "router.wasm");
  execFileSync(BIN, ["compile", join(work, "router.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  const location = { pathname: "/home" };
  const pushed = [];
  const fakeHistory = {
    pushState: (_s, _t, p) => {
      pushed.push(p);
      location.pathname = p; // the URL is now `p`
    },
  };
  let popstateFn = null;

  const root = new FakeElement("root");
  await mount(wasm, root, {
    document: fakeDocument,
    initialModel: "/",
    routeTag: "Route",
    location,
    history: fakeHistory,
    onPopState: (fn) => {
      popstateFn = fn;
    },
  });

  // The initial route is delivered from the URL.
  ok(querySelector(root, "span").textContent === "/home", "the initial path is delivered into the view");

  // A Nav (button -> Go) pushes history AND re-renders for the new path.
  querySelector(root, "button").dispatchEvent({ type: "click" });
  ok(pushed.length === 1 && pushed[0] === "/about", "navigate() pushes the new URL onto history");
  ok(querySelector(root, "span").textContent === "/about", "the view re-renders for the navigated path");

  // Back button: a popstate re-delivers the (now reverted) route.
  location.pathname = "/home";
  ok(typeof popstateFn === "function", "the shell registered a popstate listener");
  popstateFn();
  ok(querySelector(root, "span").textContent === "/home", "a popstate (back) re-delivers the route");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-ROUTING FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-ROUTING OK");

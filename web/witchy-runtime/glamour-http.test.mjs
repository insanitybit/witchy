#!/usr/bin/env node
// RFC-0015 Phase C: prove the async HTTP effect. A rune's `update` returns
// `http_get("/data", "GotData")` — it holds NO `Net`, only describes the request — and
// the shell performs the fetch with an INJECTED `opts.fetch` (a fake server), attaches a
// session credential ITSELF via `opts.authHeaders`, and dispatches the response back as
// the `GotData(status, body)` msg. We assert: the response updates the model; the request
// carried the host-attached auth header; and the rune's emitted Cmd did NOT contain the
// token (the credential never enters the WASM). Failures arrive as status 0 — ordinary
// `update` arms, not exceptions.
//
// Usage:  node web/witchy-runtime/glamour-http.test.mjs [path/to/witchy-binary]

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
from glamour import VNode, Attr, HAttr, HtmlTok, Cmd, UiRoot, UiFetch, UiRoute, UiTimer, SecretInput, SecretRef, CredentialPort
import json
from json import Json
import reflect

type Msg derive(Reflect):
    Fetch
    GotData(Int, String)

fn view(model: String) -> VNode(Msg):
    glamour.element("div", [], [
        glamour.element("button", [glamour.on("click", Fetch)], [glamour.text("load")]),
        glamour.element("span", [], [glamour.text(model)]),
    ])

fn update(model: String, msg: Msg, fetch: UiFetch) -> (String, Cmd(Msg)):
    match msg:
        Fetch -> (model, glamour.http_get(fetch, "/data", "GotData"))
        GotData(status, body) -> ("\${status}: " + body, NoCmd)

fn parse_model(j: Json) -> String:
    json.as_string(j).unwrap_or("loading")

fn arg_int(j: Json, i: Int) -> Int:
    match json.get(j, "$values"):
        Some(arr) ->
            match json.index(arr, i):
                Some(v) -> json.as_int(v).unwrap_or(0)
                None -> 0
        None -> 0

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
            if v == "GotData":
                GotData(arg_int(j, 0), arg_str(j, 1))
            else:
                Fetch
        None -> Fetch

fn model_to_json(model: String) -> Json:
    json.from_value(model)

fn msg_to_json(m: Msg) -> Json:
    json.from_value(m)

pub fn export_step(ui: UiRoot, input: String) -> String:
    let fetch = glamour.fetch_scope(ui, "fetcher", "GET", "/")
    let upd = fn(m: String, msg: Msg): update(m, msg, fetch)
    glamour.step_with(input, view, upd, parse_model, parse_msg, model_to_json, msg_to_json)

fn main(console: Console, ui: UiRoot):
    print(console, export_step(ui, "{\\"model\\": \\"loading\\"}") + "\\n")
`;

let failures = 0;
const ok = (cond, msg) => {
  console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`);
  if (!cond) failures++;
};
const tick = () => new Promise((r) => setTimeout(r, 0));

const work = mkdtempSync(join(tmpdir(), "glamour-http-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  writeFileSync(join(work, "fetcher.witchy"), RUNE);
  const wasmPath = join(work, "fetcher.wasm");
  execFileSync(BIN, ["compile", join(work, "fetcher.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  const calls = [];
  const fakeFetch = (url, init) => {
    calls.push({ url, init });
    return Promise.resolve({ status: 200, text: () => Promise.resolve("HELLO_FROM_SERVER") });
  };

  const root = new FakeElement("root");
  const app = await mount(wasm, root, {
    document: fakeDocument,
    initialModel: "loading",
    fetch: fakeFetch,
    authHeaders: () => ({ authorization: "Bearer SECRET_TOKEN" }),
    // (RFC-0040) the app's `export_step` takes a `UiRoot`; stage its `[user_caps]`
    // grant so the host mints it (bare policy data, the browser mirror of --grants).
    instantiateOpts: { userCaps: [["fetcher"]] },
  });

  ok(querySelector(root, "span").textContent === "loading", "initial model renders (loading)");

  // Click -> Fetch -> the shell performs the injected fetch.
  querySelector(root, "button").dispatchEvent({ type: "click" });
  ok(calls.length === 1 && calls[0].url === "/data", "a click issues the GET request the rune described");
  ok(calls[0].init.method === "GET", "the request method is GET");
  // The credential is attached BY THE SHELL — never by the rune.
  ok(calls[0].init.headers.authorization === "Bearer SECRET_TOKEN", "the host shell attached the session credential");
  ok(!JSON.stringify(app.getModel()).includes("SECRET_TOKEN"), "the token never entered the rune's model/state");

  // The async response dispatches back as GotData and updates the model.
  await tick();
  await tick();
  ok(querySelector(root, "span").textContent === "200: HELLO_FROM_SERVER", "the response updates the model via the GotData msg");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-HTTP FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-HTTP OK");
process.exit(0);

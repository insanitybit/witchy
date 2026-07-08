#!/usr/bin/env node
// RFC-0015 Phase D: prove the host-shell PORT effect — the mechanism behind session login
// and the WebAuthn passkey ceremony. A rune's `update` returns `port("passkeyLogin", "",
// "LoggedIn")`; the rune holds no credential and can't touch `navigator.credentials` — it
// only describes the ceremony. The shell runs it via an INJECTED `opts.ports.passkeyLogin`
// (the real one would call `navigator.credentials`), and dispatches only the OUTCOME back as
// `LoggedIn(who)`. We assert: the port runs on the login click; the rune's state becomes the
// signed-in identity; and no credential/token ever entered the rune (it sees only "alice").
//
// Usage:  node web/witchy-runtime/glamour-port.test.mjs [path/to/witchy-binary]

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
  removeAttribute(n) { this.attributes.delete(n); }
  addEventListener(e, fn) { if (!this.listeners.has(e)) this.listeners.set(e, new Set()); this.listeners.get(e).add(fn); }
  removeEventListener(e, fn) { const s = this.listeners.get(e); if (s) s.delete(fn); }
  dispatchEvent(ev) { const s = this.listeners.get(ev.type); if (s) for (const fn of [...s]) fn(ev); return true; }
  get textContent() { let o = ""; for (const c of this.childNodes) o += c.textContent; return o; }
}
const fakeDocument = { createElement: (t) => new FakeElement(t), createTextNode: (t) => new FakeText(t) };
function querySelector(node, tag) {
  if (node instanceof FakeElement && node.el === tag) return node;
  for (const c of node.childNodes) { const f = (c instanceof FakeElement || c instanceof FakeText) ? querySelector(c, tag) : null; if (f) return f; }
  return null;
}

const RUNE = `
import glamour
from glamour import VNode, Attr, HAttr, HtmlTok, Cmd, UiRoot, UiFetch, UiRoute, UiTimer, SecretInput, SecretRef, CredentialPort
import json
from json import Json
import reflect

type Msg derive(Reflect):
    Login
    LoggedIn(String)

fn view(session: String) -> VNode(Msg):
    if string.is_empty(session):
        glamour.element("div", [], [glamour.element("button", [glamour.on("click", Login)], [glamour.text("sign in with passkey")])])
    else:
        glamour.element("div", [], [glamour.element("span", [glamour.prop("class", "session-id")], [glamour.text("signed in as " + session)])])

fn update(session: String, msg: Msg, cred: CredentialPort) -> (String, Cmd(Msg)):
    match msg:
        Login -> (session, glamour.port(cred, "", "LoggedIn"))
        LoggedIn(who) -> (who, NoCmd)

fn parse_model(j: Json) -> String:
    json.as_string(j).unwrap_or("")

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
            if v == "LoggedIn":
                LoggedIn(arg_str(j, 0))
            else:
                Login
        None -> Login

fn model_to_json(s: String) -> Json:
    json.from_value(s)

fn msg_to_json(m: Msg) -> Json:
    json.from_value(m)

pub fn export_step(ui: UiRoot, input: String) -> String:
    let cred = glamour.credential_port(ui, "passkeyLogin")
    let upd = fn(s: String, msg: Msg): update(s, msg, cred)
    glamour.step_with(input, view, upd, parse_model, parse_msg, model_to_json, msg_to_json)

fn main(console: Console, ui: UiRoot):
    print(console, export_step(ui, "{\\"model\\": \\"\\"}") + "\\n")
`;

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };
const tick = () => new Promise((r) => setTimeout(r, 0));

const work = mkdtempSync(join(tmpdir(), "glamour-port-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  writeFileSync(join(work, "login.witchy"), RUNE);
  const wasmPath = join(work, "login.wasm");
  execFileSync(BIN, ["compile", join(work, "login.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  // The injected WebAuthn/session port. The REAL one runs `navigator.credentials` and keeps
  // the bearer token in the host closure; it returns only the signed-in identity.
  const portCalls = [];
  const ports = { passkeyLogin: (arg) => { portCalls.push(arg); return Promise.resolve("alice"); } };

  const root = new FakeElement("root");
  // (RFC-0039/0040) the rune's `export_step` takes a `UiRoot`; stage its grant so the host
  // mints it and Glamour can narrow the `passkeyLogin` credential port.
  const app = await mount(wasm, root, { document: fakeDocument, initialModel: "", ports, instantiateOpts: { userCaps: [["login"]] } });

  ok(querySelector(root, "button") !== null && querySelector(root, "button").textContent.includes("sign in"), "anon state shows the sign-in button");

  querySelector(root, "button").dispatchEvent({ type: "click" });
  ok(portCalls.length === 1, "the login click runs the passkey port");
  await tick();
  await tick();
  ok(querySelector(root, "span") !== null && querySelector(root, "span").textContent === "signed in as alice", "the signed-in identity renders after the ceremony");
  ok(app.getModel() === "alice", "the rune's state is just the identity — no token/credential entered it");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-PORT FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-PORT OK");
process.exit(0);

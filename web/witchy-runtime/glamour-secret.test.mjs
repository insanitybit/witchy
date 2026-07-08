#!/usr/bin/env node
// RFC-0039: host-owned SECRET input, end to end and headless. A login rune renders a
// password field with `glamour.secret_input(input, "PwStatus")` and, on submit, emits
// `glamour.submit_secret(glamour.secret_ref(input), cred, "Done")`. The rune holds NEITHER
// the bytes NOR any DOM/credential authority — it only describes the field and the submit.
// The host shell keeps the typed value in its OWN custody, hands it to an injected host
// port on submit, and feeds back only the port's result. We assert: the real password
// reaches the host port; the rune's model/wire NEVER contain it (only a non-sensitive
// "NonEmpty" status and the port result); and the secret is unrecoverable from anything the
// rune produced. This is the "a sibling cannot read another component's password" property,
// made observable — reinforced structurally by the sealed `SecretInput`/`SecretRef` caps.
//
// Usage:  node web/witchy-runtime/glamour-secret.test.mjs [path/to/witchy-binary]

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
function qsa(node, tag, acc = []) {
  if (node instanceof FakeElement && node.el === tag) acc.push(node);
  for (const c of node.childNodes) if (c instanceof FakeElement) qsa(c, tag, acc);
  return acc;
}
const tick = () => new Promise((r) => setTimeout(r, 0));
const settle = async () => { await tick(); await tick(); };

const RUNE = `
import glamour
from glamour import VNode, Attr, HAttr, HtmlTok, Cmd, UiRoot, UiFetch, UiRoute, UiTimer, SecretInput, SecretRef, CredentialPort
import json
from json import Json
import reflect

type Msg derive(Reflect):
    PwStatus(String)
    Submit
    Done(String)

fn view(model: String, input: SecretInput) -> VNode(Msg):
    glamour.element("form", [], [
        glamour.secret_input(input, "PwStatus"),
        glamour.element("button", [glamour.on("click", Submit)], [glamour.text("sign in")]),
        glamour.element("span", [], [glamour.text(model)]),
    ])

fn update(model: String, msg: Msg, input: SecretInput, cred: CredentialPort) -> (String, Cmd(Msg)):
    match msg:
        PwStatus(s) -> ("status:" + s, NoCmd)
        Submit -> (model, glamour.submit_secret(glamour.secret_ref(input), cred, "Done"))
        Done(r) -> ("result:" + r, NoCmd)

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
            if v == "PwStatus":
                PwStatus(arg_str(j, 0))
            else if v == "Done":
                Done(arg_str(j, 0))
            else:
                Submit
        None -> Submit

fn model_to_json(s: String) -> Json:
    json.from_value(s)

fn msg_to_json(m: Msg) -> Json:
    json.from_value(m)

pub fn export_step(ui: UiRoot, raw: String) -> String:
    let input = glamour.secret_field(ui, "login", "password")
    let cred = glamour.credential_port(ui, "passkeyLogin")
    let v = fn(m: String): view(m, input)
    let u = fn(m: String, msg: Msg): update(m, msg, input, cred)
    glamour.step_with(raw, v, u, parse_model, parse_msg, model_to_json, msg_to_json)

fn main(console: Console, ui: UiRoot):
    print(console, export_step(ui, "{\\"model\\": \\"\\"}") + "\\n")
`;

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

const PASSWORD = "hunter2-SECRET";

const work = mkdtempSync(join(tmpdir(), "glamour-secret-"));
try {
  copyFileSync(join(REPO, "projects/glamour/src/glamour.witchy"), join(work, "glamour.witchy"));
  writeFileSync(join(work, "login.witchy"), RUNE);
  const wasmPath = join(work, "login.wasm");
  execFileSync(BIN, ["compile", join(work, "login.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  // The injected host credential port. The REAL one would hand the password to a WebAuthn /
  // session ceremony; here it records what it received and returns an identity.
  const received = [];
  const ports = { passkeyLogin: (secret) => { received.push(secret); return Promise.resolve("alice"); } };

  const root = new FakeElement("root");
  const app = await mount(wasm, root, {
    document: fakeDocument,
    initialModel: "",
    ports,
    // (RFC-0039/0040) the rune's export takes a `UiRoot`; stage its grant so Glamour can
    // narrow the `SecretInput` (render authority) and the `passkeyLogin` credential port.
    instantiateOpts: { userCaps: [["login"]] },
  });

  // The password field renders as an inert <input type=password> — no value attribute.
  const field = qsa(root, "input")[0];
  ok(field != null && field.getAttribute("type") === "password", "the secret field renders as a password input");
  ok(field.getAttribute("value") == null, "the rendered field carries NO value (bytes are host-side only)");

  // Type a password. The host shell records it in its OWN custody and dispatches only a
  // NON-sensitive status to the rune.
  field.dispatchEvent({ type: "input", target: { value: PASSWORD } });
  await settle();
  ok(qsa(root, "span")[0].textContent === "status:NonEmpty", "typing dispatches only a non-sensitive status to the rune");
  ok(!JSON.stringify(app.getModel()).includes(PASSWORD), "the password is NOT in the rune's model after typing");
  ok(!root.textContent.includes(PASSWORD), "the password is NOT anywhere in the rendered view");

  // Submit. The host port receives the ACTUAL password; the rune gets only the result.
  qsa(root, "button")[0].dispatchEvent({ type: "click" });
  await settle();
  ok(received.length === 1 && received[0] === PASSWORD, "the host credential port receives the real password on submit");
  ok(qsa(root, "span")[0].textContent === "result:alice", "the rune sees only the port RESULT (the identity), never the secret");
  ok(!JSON.stringify(app.getModel()).includes(PASSWORD), "the password never entered the rune's model, even after submit");
  ok(!root.textContent.includes(PASSWORD), "the password never appears in the view, even after submit");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-SECRET FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-SECRET OK");
process.exit(0);

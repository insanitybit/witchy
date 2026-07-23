#!/usr/bin/env node
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { MessageChannel as NodeMessageChannel } from "node:worker_threads";

import {
  probeSandboxFetch,
  sandboxContentSecurityPolicy,
  sandboxSrcdoc,
} from "../witchy-cell-sandbox.js";

globalThis.MessageChannel ||= NodeMessageChannel;
globalThis.crypto ||= {
  getRandomValues(bytes) {
    for (let i = 0; i < bytes.length; i++) bytes[i] = i + 1;
    return bytes;
  },
};
globalThis.btoa ||= (value) => Buffer.from(value, "binary").toString("base64");

const frameSource = readFileSync(
  new URL("../witchy-cell-frame.js", import.meta.url),
  "utf8",
);
const frameHash = createHash("sha256").update(frameSource).digest("base64");
for (const path of ["../docs.html", "../rfc0103-browser-probe.html"]) {
  const page = readFileSync(new URL(path, import.meta.url), "utf8");
  assert.match(page, new RegExp(`'sha256-${frameHash.replaceAll("+", "\\+")}'`));
}

const policy = sandboxContentSecurityPolicy(
  { fetch: { origins: ["https://allowed.example"] } },
  "unit-test",
);
assert.match(policy, /connect-src https:\/\/allowed\.example:443(?:;|$)/);
assert.doesNotMatch(policy, /blocked\.example/);
assert.match(policy, /script-src 'nonce-unit-test'/);
assert.match(policy, /script-src [^;]*blob:/);
assert.doesNotMatch(policy, /data:/);
assert.match(policy, /frame-src 'none'/);

const srcdoc = sandboxSrcdoc(
  undefined,
  "unit-test",
  frameSource,
);
assert.match(srcdoc, /Content-Security-Policy/);
assert.match(srcdoc, /nonce="unit-test"/);
assert.match(srcdoc, /data-ready-token="unit-test"/);
assert.doesNotMatch(srcdoc, /allow-same-origin/);
assert.match(srcdoc, /trustedParent/);

let capturedFrame;
let capturedMessage;
const viewListeners = new Map();
const defaultView = {
  addEventListener(name, listener) { viewListeners.set(name, listener); },
  removeEventListener(name, listener) {
    if (viewListeners.get(name) === listener) viewListeners.delete(name);
  },
};
class FakeFrame {
  constructor() {
    this.attributes = new Map();
    this.listeners = new Map();
    this.parentNode = null;
    this.contentWindow = {
      postMessage: (message, target, ports) => {
        capturedMessage = { message, target };
        ports[0].postMessage({
          type: "witchy-cell-result-v1",
          result: { ok: false, text: "blocked by the synthetic browser" },
        });
      },
    };
  }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name); }
  addEventListener(name, listener) { this.listeners.set(name, listener); }
  remove() { this.parentNode = null; }
}
const body = {
  appendChild(frame) {
    capturedFrame = frame;
    frame.parentNode = body;
    queueMicrotask(() => {
      const listener = viewListeners.get("message");
      const token = frame.srcdoc.match(/data-ready-token="([^"]+)"/)[1];
      listener({
        source: {},
        data: { type: "witchy-cell-ready-v1", token },
      });
      listener({
        source: frame.contentWindow,
        data: { type: "witchy-cell-ready-v1", token: "forged" },
      });
      assert.equal(capturedMessage, undefined);
      listener({
        source: frame.contentWindow,
        data: { type: "witchy-cell-ready-v1", token },
      });
    });
  },
};
const document = {
  body,
  defaultView,
  createElement(name) {
    assert.equal(name, "iframe");
    return new FakeFrame();
  },
};

const result = await probeSandboxFetch(
  "https://blocked.example/leak",
  { fetch: { origins: ["https://allowed.example"] } },
  {
    document,
    frameSource,
    timeoutMs: 1_000,
  },
);
assert.equal(result.ok, false);
assert.equal(capturedFrame.getAttribute("sandbox"), "allow-scripts");
assert.equal(capturedFrame.getAttribute("hidden"), "");
assert.doesNotMatch(capturedFrame.getAttribute("sandbox"), /allow-same-origin/);
assert.match(capturedFrame.srcdoc, /connect-src https:\/\/allowed\.example:443/);
assert.equal(capturedMessage.target, "*");
assert.equal(capturedMessage.message.action, "probe-fetch");
assert.equal(capturedMessage.message.url, "https://blocked.example/leak");
assert.equal(
  capturedMessage.message.token,
  capturedFrame.srcdoc.match(/data-ready-token="([^"]+)"/)[1],
);

console.log("WITCHY-CELL-SANDBOX OK");

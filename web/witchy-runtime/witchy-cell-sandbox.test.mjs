#!/usr/bin/env node
import assert from "node:assert/strict";
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

const policy = sandboxContentSecurityPolicy(
  { fetch: { origins: ["https://allowed.example"] } },
  "unit-test",
);
assert.match(policy, /connect-src https:\/\/allowed\.example:443(?:;|$)/);
assert.doesNotMatch(policy, /blocked\.example/);
assert.match(policy, /script-src 'nonce-unit-test' 'wasm-unsafe-eval' data:/);
assert.match(policy, /frame-src 'none'/);

const srcdoc = sandboxSrcdoc(undefined, "unit-test");
assert.match(srcdoc, /Content-Security-Policy/);
assert.match(srcdoc, /nonce="unit-test"/);
assert.doesNotMatch(srcdoc, /allow-same-origin/);

let capturedFrame;
let capturedMessage;
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
    queueMicrotask(() => frame.listeners.get("load")());
  },
};
const document = {
  body,
  createElement(name) {
    assert.equal(name, "iframe");
    return new FakeFrame();
  },
};

const result = await probeSandboxFetch(
  "https://blocked.example/leak",
  { fetch: { origins: ["https://allowed.example"] } },
  { document, timeoutMs: 1_000 },
);
assert.equal(result.ok, false);
assert.equal(capturedFrame.getAttribute("sandbox"), "allow-scripts");
assert.equal(capturedFrame.getAttribute("hidden"), "");
assert.doesNotMatch(capturedFrame.getAttribute("sandbox"), /allow-same-origin/);
assert.match(capturedFrame.srcdoc, /connect-src https:\/\/allowed\.example:443/);
assert.equal(capturedMessage.target, "*");
assert.equal(capturedMessage.message.action, "probe-fetch");
assert.equal(capturedMessage.message.url, "https://blocked.example/leak");

console.log("WITCHY-CELL-SANDBOX OK");

import assert from "node:assert/strict";
import { installFrameCompartments } from "./glamour-frame.mjs";

class Port {
  constructor() {
    this.peer = null;
    this.onmessage = null;
    this.closed = false;
  }
  postMessage(data) {
    if (!this.closed) this.peer?.onmessage?.({ data });
  }
  start() {}
  close() { this.closed = true; }
}

class Channel {
  constructor() {
    this.port1 = new Port();
    this.port2 = new Port();
    this.port1.peer = this.port2;
    this.port2.peer = this.port1;
  }
}

class Frame {
  constructor() {
    this.localName = "iframe";
    this.attributes = new Map([["sandbox", "allow-scripts"]]);
    this.listeners = new Map();
    this.transfers = [];
    this.contentWindow = {
      postMessage: (message, target, ports) => this.transfers.push({ message, target, port: ports[0] }),
    };
  }
  getAttribute(name) { return this.attributes.has(name) ? this.attributes.get(name) : null; }
  setAttribute(name, value) { this.attributes.set(name, value); }
  addEventListener(name, listener) { this.listeners.set(name, listener); }
  removeEventListener(name, listener) { if (this.listeners.get(name) === listener) this.listeners.delete(name); }
  load() { this.listeners.get("load")?.(); }
}

const record = Object.freeze({
  node: 7,
  eventPlan: 11,
  renderer: "document.v1",
  maxGrantBytes: 65_536,
  maxEventBytes: 4_096,
  grant: "A bounded document",
  artifact: `glamour-frame1-${"a".repeat(64)}`,
  url: "/assets/frame-0123456789abcdef.html",
  nonce: `glamour-frame-nonce1-${"b".repeat(64)}`,
});
const frame = new Frame();
let live = frame;
const dispatches = [];
const errors = [];
const host = installFrameCompartments({
  frames: [record],
  resolveNode: () => live,
  dispatch: (event) => dispatches.push(event),
  onError: (error) => errors.push(error),
  MessageChannel: Channel,
});

host.sync();
assert.equal(frame.getAttribute("src"), record.url);
assert.equal(frame.transfers.length, 0);
frame.load();
assert.equal(frame.transfers.length, 1);
assert.equal(frame.transfers[0].target, "*");
assert.deepEqual(frame.transfers[0].message, {
  schema: "witchy.glamour.frame-init.v1",
  renderer: record.renderer,
  nonce: record.nonce,
});
const child = frame.transfers[0].port;
let grant;
child.onmessage = (event) => { grant = event.data; };
child.postMessage({ schema: "witchy.glamour.frame-ready.v1", renderer: record.renderer, nonce: record.nonce });
assert.deepEqual(grant, {
  schema: "witchy.glamour.frame-grant.v1",
  renderer: record.renderer,
  nonce: record.nonce,
  grant: record.grant,
});
child.postMessage({ schema: "witchy.glamour.frame-event.v1", renderer: record.renderer, nonce: record.nonce, value: "activate" });
assert.deepEqual(dispatches, [{
  plan: record.eventPlan,
  node: record.node,
  name: "glamour-frame",
  value: "activate",
  checked: false,
  key: "",
  composing: false,
  userActivation: true,
}]);
assert.deepEqual(errors, []);

live = null;
host.sync();
assert.equal(frame.listeners.has("load"), false);
assert.equal(child.peer.closed, true);
host.dispose();

const malformedFrame = new Frame();
const malformedErrors = [];
const malformed = installFrameCompartments({
  frames: [record],
  resolveNode: () => malformedFrame,
  dispatch() {},
  onError: (error) => malformedErrors.push(error),
  MessageChannel: Channel,
});
malformed.sync();
malformedFrame.load();
const malformedChild = malformedFrame.transfers[0].port;
malformedChild.postMessage({ schema: "witchy.glamour.frame-ready.v1", renderer: record.renderer, nonce: record.nonce });
malformedChild.postMessage({ schema: "witchy.glamour.frame-event.v1", renderer: record.renderer, nonce: "wrong", value: "activate" });
assert.equal(malformedErrors.length, 1);
malformed.dispose();

console.log("glamour frame host tests passed");

#!/usr/bin/env node

import assert from "node:assert/strict";
import { createHashRouting } from "./docs-routing.js";

function fakeWindow(initialHash = "") {
  const listeners = new Map();
  let hash = initialHash;
  return {
    location: {
      get hash() {
        return hash;
      },
      set hash(value) {
        hash = String(value).startsWith("#") ? String(value) : `#${value}`;
      },
    },
    addEventListener(name, listener) {
      listeners.set(name, listener);
    },
    removeEventListener(name, listener) {
      if (listeners.get(name) === listener) listeners.delete(name);
    },
    emit(name, event = {}) {
      listeners.get(name)?.(event);
    },
    hasListener(name) {
      return listeners.has(name);
    },
  };
}

const browser = fakeWindow();
const routing = createHashRouting(browser);
assert.equal(routing.path(), "/");

let routeEvents = 0;
const unsubscribe = routing.onPopState(() => routeEvents++);
routing.history.pushState({}, "", "/p/tour");
assert.equal(browser.location.hash, "#/p/tour");
assert.equal(routing.location.pathname, "/p/tour");
browser.emit("hashchange", { newURL: "https://example.test/#/p/tour" });
assert.equal(routeEvents, 0, "Glamour already dispatched a programmatic route");

browser.location.hash = "/p/back";
browser.emit("hashchange", { newURL: "https://example.test/#/p/back" });
assert.equal(routeEvents, 1, "Back, Forward, and manual hash edits dispatch");

routing.history.pushState({}, "", "/p/a");
routing.history.pushState({}, "", "/p/b");
browser.emit("hashchange", { newURL: "https://example.test/#/p/a" });
browser.emit("hashchange", { newURL: "https://example.test/#/p/b" });
assert.equal(routeEvents, 1, "rapid programmatic routes are each suppressed");

unsubscribe();
assert.equal(browser.hasListener("hashchange"), false);
console.log("DOCS-ROUTING OK");

import assert from "node:assert/strict";
import { createStorageEffectHandler } from "./glamour-storage.mjs";

const digest = "a".repeat(64);
const policy = {
  kind: "storage",
  provider: "local",
  namespace: "preferences",
  keyPrefix: "book.",
  maxValueBytes: 16,
};
const artifact = {
  grantDigest: digest,
  effectDescriptors: {
    1: { semantic: "storage-get", policy },
    2: { semantic: "storage-set", policy },
    3: { semantic: "storage-remove", policy },
  },
};
const values = new Map();
const local = {
  getItem: (key) => values.get(key) ?? null,
  setItem: (key, value) => values.set(key, value),
  removeItem: (key) => values.delete(key),
};
const field = (value) => `${new TextEncoder().encode(value).byteLength}:${value}`;
const request = (key, value) => ["local", "preferences", "book.", "16", key, value]
  .filter((entry) => entry !== undefined)
  .map(field)
  .join("");
const handle = createStorageEffectHandler({ artifact, local: () => local });
const physicalKey = `witchy:${digest}:preferences:book.theme`;

assert.deepEqual(handle({ descriptor: 1, request: request("book.theme") }), { kind: "missing" });
assert.equal(handle({ descriptor: 2, request: request("book.theme", "dark") }), undefined);
assert.equal(values.get(physicalKey), "dark");
assert.deepEqual(
  handle({ descriptor: 1, request: request("book.theme") }),
  { kind: "value", value: "dark" },
);
assert.equal(handle({ descriptor: 3, request: request("book.theme") }), undefined);
assert.equal(values.has(physicalKey), false);
assert.throws(() => handle({ descriptor: 1, request: request("other.theme") }), /exceeds/);
assert.throws(() => handle({ descriptor: 2, request: request("book.theme", "x".repeat(17)) }), /exceeds/);
assert.throws(
  () => createStorageEffectHandler({ artifact: { ...artifact, grantDigest: "forged" } }),
  /digest/,
);

console.log("GLAMOUR-STORAGE OK");

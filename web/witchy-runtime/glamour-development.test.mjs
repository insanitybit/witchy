#!/usr/bin/env node

import assert from "node:assert/strict";
import { installDevelopmentSwap } from "./glamour-development.mjs";
import { FakeElement, fakeDocument } from "./glamour-test-dom.mjs";

const currentContract = {
  applicationIdentity: "app",
  modelSchema: "11".repeat(32),
  authorizationSchema: "22".repeat(32),
  templateSchema: "templates",
  snapshotFormat: 2,
  maxSnapshotBytes: 1024,
  migrationSchemas: [],
};
const candidateContract = {
  ...currentContract,
  modelSchema: "33".repeat(32),
  migrationSchemas: [currentContract.modelSchema],
};
const manifest = {
  development: currentContract,
};
const next = {
  ...candidateContract,
  decision: "swap",
  reason: "all authenticated compatibility identities match",
  buildId: "next",
  wasm: "/assets/next.wasm",
  manifest: "/witchy-web-manifest.json",
};
const snapshot = Uint8Array.of(1, 2, 3, 4);
let oldDisposals = 0;
const old = {
  developmentMetadata: {
    modelSchema: currentContract.modelSchema,
    authorizationSchema: currentContract.authorizationSchema,
  },
  snapshot: () => snapshot.slice(),
  inspectDevelopment: () => Object.freeze({
    schema: "witchy.glamour.devtools.v1",
    application: "old",
  }),
  dispose: () => {
    oldDisposals += 1;
  },
};
const root = new FakeElement("root");
let candidateMode = "fail";
let candidateActivations = 0;
let candidateDisposals = 0;
const mountOptimized = async (_bytes, detached, options) => {
  assert.deepEqual(options.restoreSnapshot, snapshot);
  assert.equal(options.deferActivation, true);
  assert.equal(detached.tag, "div");
  if (candidateMode === "fail") throw new Error("restore rejected");
  return {
    developmentMetadata: {
      modelSchema: candidateContract.modelSchema,
      authorizationSchema: candidateContract.authorizationSchema,
    },
    inspectDevelopment: () => Object.freeze({
      schema: "witchy.glamour.devtools.v1",
      application: "candidate",
    }),
    activate: (target) => {
      assert.equal(target, root);
      candidateActivations += 1;
    },
    dispose: () => {
      candidateDisposals += 1;
    },
  };
};

globalThis.document = fakeDocument;
globalThis.fetch = async (url) => {
  if (url.endsWith(".wasm")) {
    return { ok: true, arrayBuffer: async () => new ArrayBuffer(8) };
  }
  return { ok: true, json: async () => ({ development: candidateContract }) };
};

const bridge = installDevelopmentSwap({
  application: old,
  root,
  manifest,
  mountOptimized,
  instantiateOptions: { userCaps: [["app"]] },
});
assert.equal("application" in bridge, false, "the bridge exposes no dispatch-capable app handle");
assert.equal("manifest" in bridge, false, "the bridge exposes no mutable manifest handle");
assert.deepEqual(bridge.inspect(), {
  schema: "witchy.glamour.devtools.v1",
  application: "old",
});
await assert.rejects(bridge.swap(next), /restore rejected/);
assert.equal(oldDisposals, 0, "failed candidate leaves old application live");
assert.equal(candidateActivations, 0);

candidateMode = "pass";
const result = await bridge.swap(next);
assert.equal(result.restoredBytes, snapshot.byteLength);
assert.equal(oldDisposals, 1, "old application disposes only after candidate restore");
assert.equal(candidateActivations, 1);
assert.equal(candidateDisposals, 0);
assert.deepEqual(bridge.inspect(), {
  schema: "witchy.glamour.devtools.v1",
  application: "candidate",
});

console.log("GLAMOUR-DEVELOPMENT OK");

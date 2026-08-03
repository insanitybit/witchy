#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { mountOptimized } from "./glamour-optimized.mjs";
import { FrameKind, encodeOutputFrame } from "./glamour-protocol.mjs";
import { FakeElement, fakeDocument } from "./glamour-test-dom.mjs";
import { instantiate } from "./witchy-runtime.mjs";

const BIN = resolve(process.argv[2] || "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const PROJECT = join(REPO, "projects/glamour/examples/optimized_effects");
const APP_ID = 7;
const BUILD_ID = 0x0102_0304_0506_0708n;

function deferred() {
  let resolvePromise;
  const promise = new Promise((resolve) => {
    resolvePromise = resolve;
  });
  return { promise, resolve: resolvePromise };
}

async function flushCompletions() {
  await Promise.resolve();
  await Promise.resolve();
}

const work = mkdtempSync(join(tmpdir(), "glamour-optimized-effects-wasm-"));
try {
  const wasmPath = join(work, "optimized-effects.wasm");
  execFileSync(
    BIN,
    ["compile", "src/optimized_effects.witchy", "--out", wasmPath],
    { cwd: PROJECT },
  );
  const wasmBytes = readFileSync(wasmPath);
  const firstEffect = deferred();
  const secondEffect = deferred();
  const effectRuns = [];
  let effectCancellations = 0;
  const subscriptionRuns = [];
  const subscriptionEmits = [];
  let subscriptionCancellations = 0;
  const manifest = {
    appId: APP_ID,
    buildId: BUILD_ID,
    templates: new Map(),
    nodes: new Map(),
    regions: new Map(),
    properties: new Map(),
    attributes: new Map(),
    aria: new Map(),
    eventClasses: new Map(),
    eventPlans: new Map(),
    effectDescriptors: new Map([
      [37, { handler: "request", resultSchema: 43, ownerScope: 49 }],
    ]),
    subscriptionDescriptors: new Map([
      [41, { handler: "interval", resultSchema: 47, ownerScope: 51 }],
    ]),
  };
  const startFrame = encodeOutputFrame({
    kind: FrameKind.Start,
    appId: APP_ID,
    buildId: BUILD_ID,
  });
  let runtime;
  const app = await mountOptimized(wasmBytes, new FakeElement("root"), {
    document: fakeDocument,
    manifest,
    startFrame,
    instantiateOptions: { userCaps: [["optimized-effects"]] },
    instantiate: async (bytes, options) => {
      runtime = await instantiate(bytes, options);
      return runtime;
    },
    effectHandlers: {
      request({ request, signal }) {
        effectRuns.push({ request, signal });
        return {
          promise: request === "first" ? firstEffect.promise : secondEffect.promise,
          cancel: () => {
            effectCancellations += 1;
          },
        };
      },
    },
    subscriptionHandlers: {
      interval({ request, signal, emit }) {
        subscriptionRuns.push({ request, signal });
        subscriptionEmits.push(emit);
        return () => {
          subscriptionCancellations += 1;
        };
      },
    },
  });

  assert.deepEqual(effectRuns.map((run) => run.request), ["first"]);
  assert.deepEqual(subscriptionRuns.map((run) => run.request), ["10"]);
  assert.equal(app.activeEffectCount, 1);
  assert.equal(app.activeSubscriptionCount, 1);

  subscriptionEmits[0]("tick-1");
  await flushCompletions();
  assert.deepEqual(effectRuns.map((run) => run.request), ["first", "second"]);
  assert.equal(effectCancellations, 1);
  assert.equal(effectRuns[0].signal.aborted, true);
  assert.equal(subscriptionRuns.length, 1);

  firstEffect.resolve("stale");
  await flushCompletions();
  assert.deepEqual(subscriptionRuns.map((run) => run.request), ["10"]);

  secondEffect.resolve("fresh");
  await flushCompletions();
  assert.equal(effectCancellations, 1);
  assert.deepEqual(subscriptionRuns.map((run) => run.request), ["10", "20"]);
  assert.equal(subscriptionCancellations, 1);
  assert.equal(subscriptionRuns[0].signal.aborted, true);

  subscriptionEmits[0]("stale tick");
  await flushCompletions();
  assert.equal(app.activeSubscriptionCount, 1);

  subscriptionEmits[1]("tick-2");
  await flushCompletions();
  assert.equal(app.activeSubscriptionCount, 0);
  assert.equal(subscriptionCancellations, 2);
  const pages = runtime.memory.buffer.byteLength / (64 * 1024);
  assert.ok(pages <= 8, `the effect fixture stays within eight Wasm pages (got ${pages})`);

  app.dispose();
  console.log("GLAMOUR-OPTIMIZED-EFFECTS-WASM OK");
} finally {
  rmSync(work, { recursive: true, force: true });
}

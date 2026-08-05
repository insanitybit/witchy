#!/usr/bin/env node

import assert from "node:assert/strict";
import { mountOptimized } from "./glamour-optimized.mjs";
import {
  CompletionSource,
  EffectOp,
  FrameKind,
  GLAMOUR_HEADER_BYTES,
  encodeOperation,
  encodeOutputFrame,
} from "./glamour-protocol.mjs";
import { FakeElement, fakeDocument as document } from "./glamour-test-dom.mjs";

const APP_ID = 7;
const BUILD_ID = 0x0102_0304_0506_0708n;
const encoder = new TextEncoder();
const decoder = new TextDecoder();

function effectsFrame(sequence, specifications) {
  const operations = specifications.map((specification) => {
    switch (specification.kind) {
      case "start":
        return encodeOperation(EffectOp.Start, [
          specification.instance,
          specification.cancellationKey,
          specification.descriptor,
          0,
          encoder.encode(specification.request).byteLength,
        ]);
      case "cancel":
        return encodeOperation(EffectOp.Cancel, [specification.cancellationKey]);
      case "sync":
        return encodeOperation(EffectOp.SyncSubscription, [
          specification.subscription,
          specification.descriptor,
          0,
          encoder.encode(specification.request).byteLength,
        ]);
      case "remove":
        return encodeOperation(EffectOp.RemoveSubscription, [
          specification.subscription,
        ]);
      default:
        throw new Error(`unknown effect specification ${specification.kind}`);
    }
  });
  let payloadOffset =
    GLAMOUR_HEADER_BYTES +
    operations.reduce((total, operation) => total + operation.byteLength, 0);
  const payloads = [];
  for (let index = 0; index < specifications.length; index += 1) {
    const specification = specifications[index];
    if (specification.kind !== "start" && specification.kind !== "sync") continue;
    const payload = encoder.encode(specification.request);
    const view = new DataView(operations[index].buffer);
    view.setUint32(specification.kind === "start" ? 20 : 16, payloadOffset, true);
    payloadOffset += payload.byteLength;
    payloads.push(payload);
  }
  return encodeOutputFrame({
    kind: FrameKind.Effects,
    appId: APP_ID,
    buildId: BUILD_ID,
    sequence,
    operations,
    payloads,
  });
}

function emptyFrame(sequence) {
  return encodeOutputFrame({
    kind: FrameKind.Effects,
    appId: APP_ID,
    buildId: BUILD_ID,
    sequence,
  });
}

function fakeRuntime(outputs) {
  const memory = { buffer: new ArrayBuffer(64 * 1024) };
  const inputPointer = 1024;
  const outputPointer = 8192;
  const completions = [];
  const completionPayloads = [];
  let outputLength = 0;
  let disposals = 0;
  let outputIndex = 0;
  const stage = () => {
    const frame = outputs[outputIndex++];
    assert.ok(frame, "the fixture supplied an output for every completion");
    new Uint8Array(memory.buffer).set(frame, outputPointer);
    outputLength = frame.byteLength;
    return outputPointer;
  };
  return {
    memory,
    completions,
    completionPayloads,
    get disposals() {
      return disposals;
    },
    instance: {
      exports: {
        __glamour_protocol_version: () => 1 << 16,
        __glamour_input_reserve: () => inputPointer,
        __glamour_init: stage,
        __glamour_resume: () => {
          outputLength = 0;
          return 0;
        },
        __glamour_dispatch: (pointer, length) => {
          const bytes = new Uint8Array(memory.buffer, pointer, length);
          const view = new DataView(memory.buffer, pointer, length);
          const kind = view.getUint8(8);
          if (kind === FrameKind.Activation) return stage();
          assert.equal(kind, FrameKind.EffectCompletion);
          const payloadOffset = view.getUint32(GLAMOUR_HEADER_BYTES + 32, true);
          const payloadLength = view.getUint32(GLAMOUR_HEADER_BYTES + 36, true);
          completionPayloads.push([
            ...bytes.subarray(payloadOffset, payloadOffset + payloadLength),
          ]);
          completions.push({
            source: view.getUint32(GLAMOUR_HEADER_BYTES + 8, true),
            instance: view.getUint32(GLAMOUR_HEADER_BYTES + 12, true),
            generation: view.getUint32(GLAMOUR_HEADER_BYTES + 16, true),
            descriptor: view.getUint32(GLAMOUR_HEADER_BYTES + 20, true),
            resultSchema: view.getUint32(GLAMOUR_HEADER_BYTES + 24, true),
            status: view.getUint32(GLAMOUR_HEADER_BYTES + 28, true),
            value: decoder.decode(bytes.subarray(payloadOffset, payloadOffset + payloadLength)),
          });
          return stage();
        },
        __glamour_output_length: () => outputLength,
        __glamour_output_release: () => {
          outputLength = 0;
        },
        __glamour_dispose: () => {
          disposals += 1;
        },
      },
    },
  };
}

function enableDevelopmentMetadata(runtime) {
  const pointer = 128;
  const metadata = new Uint8Array(80);
  metadata.set(encoder.encode("WGDM"), 0);
  const view = new DataView(metadata.buffer);
  view.setUint16(4, 1, true);
  view.setUint16(6, 1, true);
  view.setUint16(8, 1, true);
  view.setUint16(10, 0, true);
  view.setUint32(12, 0, true);
  metadata.fill(0x11, 16, 48);
  metadata.fill(0x22, 48, 80);
  new DataView(runtime.memory.buffer).setUint32(pointer, metadata.byteLength, true);
  new Uint8Array(runtime.memory.buffer).set(metadata, pointer + 4);
  runtime.instance.exports.__glamour_dev_metadata = () => pointer;
  runtime.instance.exports.__glamour_dev_changes = () => pointer + metadata.byteLength + 4;
  runtime.instance.exports.__glamour_dev_changes_length = () => 0;
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((onResolve, onReject) => {
    resolve = onResolve;
    reject = onReject;
  });
  return { promise, resolve, reject };
}

const firstEffect = deferred();
const secondEffect = deferred();
const effectRuns = [];
let effectCancellations = 0;
const subscriptionRuns = [];
const subscriptionEmits = [];
let subscriptionCancellations = 0;

const runtime = fakeRuntime([
  effectsFrame(0n, [
    {
      kind: "start",
      instance: 101,
      cancellationKey: 77,
      descriptor: 37,
      request: "first",
    },
    {
      kind: "sync",
      subscription: 201,
      descriptor: 41,
      request: "10",
    },
  ]),
  effectsFrame(1n, [
    {
      kind: "start",
      instance: 102,
      cancellationKey: 77,
      descriptor: 37,
      request: "second",
    },
    {
      kind: "sync",
      subscription: 201,
      descriptor: 41,
      request: "10",
    },
  ]),
  effectsFrame(2n, [
    {
      kind: "sync",
      subscription: 201,
      descriptor: 41,
      request: "20",
    },
  ]),
  effectsFrame(3n, [{ kind: "remove", subscription: 201 }]),
]);
enableDevelopmentMetadata(runtime);
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
    [37, { handler: "request", resultSchema: 43, ownerScope: 49, semantic: "resource" }],
  ]),
  subscriptionDescriptors: new Map([
    [41, { handler: "interval", resultSchema: 47, ownerScope: 51, semantic: "timer" }],
  ]),
  features: { mode: "development" },
};
const startFrame = encodeOutputFrame({
  kind: FrameKind.Start,
  appId: APP_ID,
  buildId: BUILD_ID,
});
const root = new FakeElement("root");
const app = await mountOptimized(new Uint8Array(), root, {
  document,
  manifest,
  startFrame,
  instantiate: async () => runtime,
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

assert.equal(app.activeEffectCount, 1);
assert.equal(app.activeSubscriptionCount, 1);
assert.deepEqual(effectRuns.map((run) => run.request), ["first"]);
assert.deepEqual(subscriptionRuns.map((run) => run.request), ["10"]);

subscriptionEmits[0]("tick-1");
await Promise.resolve();
await Promise.resolve();
assert.equal(runtime.completions.length, 1);
assert.deepEqual(runtime.completions[0], {
  source: CompletionSource.Subscription,
  instance: 201,
  generation: 2,
  descriptor: 41,
  resultSchema: 47,
  status: 0,
  value: "tick-1",
});
assert.deepEqual(effectRuns.map((run) => run.request), ["first", "second"]);
assert.equal(effectCancellations, 1, "a stable key cancels the prior effect generation");
assert.equal(effectRuns[0].signal.aborted, true);
assert.equal(subscriptionRuns.length, 1, "an unchanged subscription keeps its host source");

firstEffect.resolve("stale");
await Promise.resolve();
await Promise.resolve();
assert.equal(runtime.completions.length, 1, "a replaced effect cannot deliver a stale completion");

secondEffect.resolve("fresh");
await Promise.resolve();
await Promise.resolve();
assert.equal(runtime.completions.length, 2);
assert.deepEqual(runtime.completions[1], {
  source: CompletionSource.Effect,
  instance: 102,
  generation: 3,
  descriptor: 37,
  resultSchema: 43,
  status: 0,
  value: "fresh",
});
assert.equal(effectCancellations, 1, "normal completion does not invoke cancellation");
assert.deepEqual(subscriptionRuns.map((run) => run.request), ["10", "20"]);
assert.equal(subscriptionCancellations, 1);
assert.equal(subscriptionRuns[0].signal.aborted, true);

subscriptionEmits[0]("stale tick");
await Promise.resolve();
await Promise.resolve();
assert.equal(runtime.completions.length, 2, "a replaced subscription generation is inert");

subscriptionEmits[1]("tick-2");
await Promise.resolve();
await Promise.resolve();
assert.equal(runtime.completions.length, 3);
assert.equal(runtime.completions[2].generation, 4);
assert.equal(runtime.completions[2].value, "tick-2");
assert.equal(app.activeSubscriptionCount, 0);
assert.equal(subscriptionCancellations, 2);

const lifecycle = app.inspectDevelopment().hostLifecycle;
assert.deepEqual(lifecycle, [
  { kind: "effect", phase: "started", instance: 101, descriptor: 37, generation: 1, semantic: "resource" },
  { kind: "subscription", phase: "started", instance: 201, descriptor: 41, generation: 2, semantic: "timer" },
  { kind: "subscription", phase: "emitted", status: "ok", instance: 201, descriptor: 41, generation: 2, semantic: "timer" },
  { kind: "effect", phase: "cancelled", instance: 101, descriptor: 37, generation: 1, semantic: "resource" },
  { kind: "effect", phase: "started", instance: 102, descriptor: 37, generation: 3, semantic: "resource" },
  { kind: "effect", phase: "completed", status: "ok", instance: 102, descriptor: 37, generation: 3, semantic: "resource" },
  { kind: "subscription", phase: "cancelled", instance: 201, descriptor: 41, generation: 2, semantic: "timer" },
  { kind: "subscription", phase: "started", instance: 201, descriptor: 41, generation: 4, semantic: "timer" },
  { kind: "subscription", phase: "emitted", status: "ok", instance: 201, descriptor: 41, generation: 4, semantic: "timer" },
  { kind: "subscription", phase: "cancelled", instance: 201, descriptor: 41, generation: 4, semantic: "timer" },
]);
assert.ok(Object.isFrozen(lifecycle));
assert.ok(lifecycle.every(Object.isFrozen));
assert.ok(!JSON.stringify(lifecycle).includes("first"));
assert.ok(!JSON.stringify(lifecycle).includes("fresh"));
assert.ok(!JSON.stringify(lifecycle).includes("tick"));

subscriptionEmits[1]("late tick");
await Promise.resolve();
await Promise.resolve();
assert.equal(runtime.completions.length, 3);

app.dispose();
assert.equal(runtime.disposals, 1);
assert.equal(app.activeEffectCount, 0);
assert.equal(app.activeSubscriptionCount, 0);

const lifecycleLimit = 130;
const lifecycleRuntime = fakeRuntime([
  effectsFrame(
    0n,
    Array.from({ length: lifecycleLimit }, (_, index) => ({
      kind: "sync",
      subscription: index + 1,
      descriptor: 41,
      request: "",
    })),
  ),
]);
enableDevelopmentMetadata(lifecycleRuntime);
const lifecycleApp = await mountOptimized(new Uint8Array(), new FakeElement("root"), {
  document,
  manifest,
  startFrame,
  instantiate: async () => lifecycleRuntime,
  subscriptionHandlers: {
    interval() {},
  },
});
const cappedLifecycle = lifecycleApp.inspectDevelopment().hostLifecycle;
assert.equal(cappedLifecycle.length, 128);
assert.equal(cappedLifecycle[0].instance, 3);
assert.equal(cappedLifecycle.at(-1).instance, 130);
lifecycleApp.dispose();

const resumedRuntime = fakeRuntime([]);
const resumedSubscriptions = [];
let resumedSubscriptionCancellations = 0;
const resumedApp = await mountOptimized(new Uint8Array(), new FakeElement("root"), {
  document,
  manifest: {
    ...manifest,
    resume: {
      version: 1,
      sequence: 1,
      inputSequence: 0,
      nodes: [],
      regions: [],
      events: [],
      subscriptions: [{ subscription: 201, descriptor: 41, request: "10" }],
    },
  },
  startFrame,
  resume: true,
  instantiate: async () => resumedRuntime,
  subscriptionHandlers: {
    interval({ request }) {
      resumedSubscriptions.push(request);
      return () => {
        resumedSubscriptionCancellations += 1;
      };
    },
  },
});
assert.deepEqual(resumedSubscriptions, ["10"]);
assert.equal(resumedApp.activeSubscriptionCount, 1);
resumedApp.dispose();
assert.equal(resumedSubscriptionCancellations, 1);

const barrierRuntime = fakeRuntime([
  effectsFrame(1n, [
    { kind: "start", instance: 501, cancellationKey: 0, descriptor: 37, request: "startup" },
    { kind: "sync", subscription: 601, descriptor: 41, request: "25" },
  ]),
]);
const barrierEffects = [];
const barrierSubscriptions = [];
const barrierApp = await mountOptimized(new Uint8Array(), new FakeElement("root"), {
  document,
  manifest: {
    ...manifest,
    features: { mode: "production", startupBarrier: true },
    resume: {
      version: 1,
      sequence: 1,
      inputSequence: 0,
      nodes: [],
      regions: [],
      events: [],
      subscriptions: [],
    },
  },
  startFrame,
  resume: true,
  instantiate: async () => barrierRuntime,
  effectHandlers: {
    request({ request }) {
      barrierEffects.push(request);
      return new Promise(() => {});
    },
  },
  subscriptionHandlers: {
    interval({ request }) {
      barrierSubscriptions.push(request);
    },
  },
});
assert.deepEqual(barrierEffects, ["startup"]);
assert.deepEqual(barrierSubscriptions, ["25"]);
barrierApp.dispose();

let malformedStarts = 0;
const malformedRuntime = fakeRuntime([
  encodeOutputFrame({
    kind: FrameKind.Patch,
    appId: APP_ID,
    buildId: BUILD_ID,
    operations: [
      encodeOperation(EffectOp.Start, [301, 0, 37, GLAMOUR_HEADER_BYTES + 52, 0]),
      encodeOperation(3, [999, 17, GLAMOUR_HEADER_BYTES + 52, 0]),
    ],
  }),
]);
await assert.rejects(
  mountOptimized(new Uint8Array(), new FakeElement("root"), {
    document,
    manifest,
    startFrame,
    instantiate: async () => malformedRuntime,
    effectHandlers: {
      request() {
        malformedStarts += 1;
        return Promise.resolve("should not run");
      },
    },
  }),
  /unknown node 999/,
);
assert.equal(malformedStarts, 0, "a malformed DOM operation starts no effect");
assert.equal(malformedRuntime.disposals, 1);

let malformedDescriptorStarts = 0;
const malformedDescriptorRuntime = fakeRuntime([
  effectsFrame(0n, [{
    kind: "start",
    instance: 302,
    cancellationKey: 0,
    descriptor: 37,
    request: "",
  }]),
]);
await assert.rejects(
  mountOptimized(new Uint8Array(), new FakeElement("root"), {
    document,
    manifest: {
      ...manifest,
      effectDescriptors: new Map([[
        37,
        { handler: "request", resultSchema: 43 },
      ]]),
    },
    startFrame,
    instantiate: async () => malformedDescriptorRuntime,
    effectHandlers: {
      request() {
        malformedDescriptorStarts += 1;
        return Promise.resolve("should not run");
      },
    },
  }),
  /effect descriptor 37 is malformed/,
);
assert.equal(malformedDescriptorStarts, 0);
assert.equal(malformedDescriptorRuntime.disposals, 1);

const productionRuntime = fakeRuntime([
  effectsFrame(0n, [
    {
      kind: "start",
      instance: 351,
      cancellationKey: 0,
      descriptor: 37,
      request: "",
    },
  ]),
  emptyFrame(1n),
]);
const productionApp = await mountOptimized(
  new Uint8Array(),
  new FakeElement("root"),
  {
    document,
    manifest: {
      ...manifest,
      features: { mode: "production" },
      effectDescriptors: new Map([
        [37, { handler: "request", resultSchema: 43, ownerScope: 49, semantic: "http" }],
      ]),
    },
    startFrame,
    instantiate: async () => productionRuntime,
    effectHandlers: {
      request() {
        return { status: 200, body: "ok" };
      },
    },
  },
);
await Promise.resolve();
await Promise.resolve();
assert.equal(productionRuntime.completions[0].status, 0);
assert.deepEqual(
  productionRuntime.completionPayloads[0],
  [1, 0, 0, 0, 200, 0, 0, 0, 2, 0, 0, 0, 111, 107],
);
productionApp.dispose();

const boundedRuntime = fakeRuntime([
  effectsFrame(0n, [
    {
      kind: "start",
      instance: 401,
      cancellationKey: 0,
      descriptor: 37,
      request: "",
    },
  ]),
  emptyFrame(1n),
]);
const boundedApp = await mountOptimized(new Uint8Array(), new FakeElement("root"), {
  document,
  manifest: { ...manifest, limits: { maxCompletionBytes: 4 } },
  startFrame,
  instantiate: async () => boundedRuntime,
  effectHandlers: {
    request() {
      return "oversized";
    },
  },
});
await Promise.resolve();
await Promise.resolve();
assert.equal(boundedRuntime.completions[0].status, 1);
assert.equal(boundedRuntime.completions[0].value, "");
boundedApp.dispose();

console.log("GLAMOUR-OPTIMIZED-EFFECTS OK");

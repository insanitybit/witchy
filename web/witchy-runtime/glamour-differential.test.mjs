#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { mount } from "./glamour-dom.mjs";
import { mountOptimized } from "./glamour-optimized.mjs";
import {
  optimizedCounterManifest,
  optimizedCounterStartFrame,
} from "./glamour-optimized-counter-fixture.mjs";
import { createReferenceDom } from "./glamour-reference-dom.mjs";
import { FakeElement, fakeDocument } from "./glamour-test-dom.mjs";
import { instantiate } from "./witchy-runtime.mjs";

const BIN = resolve(process.argv[2] || "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const PROJECT = join(REPO, "projects/glamour/examples/optimized_counter");
const SOURCE = join(PROJECT, "src/optimized_counter.witchy");

function normalizeNode(node) {
  const tag = node.el || node.tag;
  if (!tag) return { text: node.textContent };
  return {
    tag,
    attributes: [...(node.attributes || new Map()).entries()].sort(([left], [right]) =>
      left.localeCompare(right)
    ),
    children: node.childNodes.map(normalizeNode),
  };
}

function referenceSnapshot(app, root) {
  const stats = app.getRuntimeStats();
  return {
    dom: normalizeNode(root.childNodes[0]),
    commands: stats.activeStableCommands,
    subscriptions: stats.activeSubscriptions,
    security: "no-host-work",
    lifecycle: "live",
  };
}

function optimizedSnapshot(app, root) {
  return {
    dom: normalizeNode(root.childNodes[0]),
    commands: app.activeEffectCount,
    subscriptions: app.activeSubscriptionCount,
    security: "no-host-work",
    lifecycle: "live",
  };
}

function clickReference(root) {
  root.childNodes[0].dispatchEvent({
    type: "click",
    target: root.childNodes[0],
    preventDefault() {},
  });
}

function clickOptimized(root) {
  const list = root.childNodes[0];
  root.dispatchEvent({
    type: "click",
    target: list,
    composedPath: () => [list, root],
    preventDefault() {},
  });
}

async function driveReference(wasmBytes, instantiateRuntime) {
  const dom = createReferenceDom();
  const root = dom.createRoot();
  const app = await mount(wasmBytes, root, {
    document: dom.document,
    initialModel: 0,
    instantiateOpts: { userCaps: [["optimized-counter"]] },
    instantiate: instantiateRuntime,
  });
  const trace = [referenceSnapshot(app, root)];
  for (let step = 0; step < 3; step += 1) {
    clickReference(root);
    trace.push(referenceSnapshot(app, root));
  }
  assert.equal(app.getModel(), 3);
  const lateTarget = root.childNodes[0];
  app.unmount();
  lateTarget.dispatchEvent({ type: "click", target: lateTarget });
  assert.equal(app.getModel(), 3, "a disposed JSON host ignores late events");
  return trace;
}

const work = mkdtempSync(join(tmpdir(), "glamour-differential-"));
try {
  const interpreterLines = execFileSync(BIN, ["run", SOURCE], {
    cwd: PROJECT,
    env: { ...process.env, WITCHY_INTERP: "1" },
    encoding: "utf8",
  }).trim().split("\n");
  assert.equal(interpreterLines.length, 4);
  for (const line of interpreterLines) JSON.parse(line);

  let interpreterStep = 0;
  const interpreterTrace = await driveReference(
    new Uint8Array(),
    async () => ({
      memory: { buffer: new ArrayBuffer(64 * 1024) },
      callString(_name, input) {
        const parsed = JSON.parse(input);
        assert.equal(parsed.model, Math.max(0, interpreterStep - 1));
        assert.equal("msg" in parsed, interpreterStep !== 0);
        return interpreterLines[interpreterStep++];
      },
    }),
  );
  assert.equal(interpreterStep, 4);

  const wasmPath = join(work, "optimized-counter.wasm");
  execFileSync(
    BIN,
    ["compile", "src/optimized_counter.witchy", "--out", wasmPath],
    { cwd: PROJECT },
  );
  const wasmBytes = readFileSync(wasmPath);
  const compiledReferenceTrace = await driveReference(wasmBytes, instantiate);

  const optimizedRoot = new FakeElement("root");
  const optimizedApp = await mountOptimized(wasmBytes, optimizedRoot, {
    document: fakeDocument,
    manifest: optimizedCounterManifest(),
    startFrame: optimizedCounterStartFrame(),
    instantiateOptions: { userCaps: [["optimized-counter"]] },
  });
  const optimizedTrace = [optimizedSnapshot(optimizedApp, optimizedRoot)];
  for (let step = 0; step < 3; step += 1) {
    clickOptimized(optimizedRoot);
    optimizedTrace.push(optimizedSnapshot(optimizedApp, optimizedRoot));
  }
  optimizedApp.dispose();
  const finalDom = normalizeNode(optimizedRoot.childNodes[0]);
  clickOptimized(optimizedRoot);
  assert.deepEqual(
    normalizeNode(optimizedRoot.childNodes[0]),
    finalDom,
    "a disposed optimized host ignores late events",
  );

  assert.deepEqual(
    compiledReferenceTrace,
    interpreterTrace,
    "compiled Wasm plus the JSON host matches interpreter plus the JSON host",
  );
  assert.deepEqual(
    optimizedTrace,
    interpreterTrace,
    "the optimized binary host matches the normalized JSON oracle after every message",
  );
  console.log("GLAMOUR-DIFFERENTIAL OK");
} finally {
  rmSync(work, { recursive: true, force: true });
}

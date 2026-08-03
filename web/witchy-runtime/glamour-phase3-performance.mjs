#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import { mount } from "./glamour-dom.mjs";
import { mountOptimized } from "./glamour-optimized.mjs";
import {
  optimizedCounterManifest,
  optimizedCounterStartFrame,
} from "./glamour-optimized-counter-fixture.mjs";
import { createReferenceDom } from "./glamour-reference-dom.mjs";
import { FakeElement, fakeDocument } from "./glamour-test-dom.mjs";

const BIN = resolve(process.argv[2] || "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const PROJECT = join(REPO, "projects/glamour/examples/optimized_counter");
const EVENTS = 100;
const WARMUPS = 3;
const SAMPLES = 30;

function percentile(values, fraction) {
  const ordered = [...values].sort((left, right) => left - right);
  return Number(
    ordered[Math.max(0, Math.ceil(ordered.length * fraction) - 1)].toFixed(3),
  );
}

function summary(samples, field) {
  const values = samples.map((sample) => sample[field]);
  return {
    median: percentile(values, 0.5),
    p95: percentile(values, 0.95),
  };
}

async function sampleReference(wasmBytes) {
  const dom = createReferenceDom();
  const root = dom.createRoot();
  const mountStart = performance.now();
  const app = await mount(wasmBytes, root, {
    document: dom.document,
    initialModel: 0,
    instantiateOpts: { userCaps: [["optimized-counter"]] },
  });
  const mounted = performance.now();
  const list = root.childNodes[0];
  for (let event = 0; event < EVENTS; event += 1) {
    list.dispatchEvent({ type: "click", target: list, preventDefault() {} });
  }
  const completed = performance.now();
  assert.equal(app.getModel(), EVENTS);
  const runtime = app.getRuntimeStats();
  const operations = dom.snapshotOperations();
  app.unmount();
  return {
    mountMs: mounted - mountStart,
    interactionMs: completed - mounted,
    transportBytes: runtime.protocol.inputBytes + runtime.protocol.outputBytes,
    wasmMemoryPages: runtime.wasmMemoryPages,
    domOperations: Object.values(operations).reduce((sum, count) => sum + count, 0),
  };
}

async function sampleOptimized(wasmBytes) {
  const root = new FakeElement("root");
  const mountStart = performance.now();
  const app = await mountOptimized(wasmBytes, root, {
    document: fakeDocument,
    manifest: optimizedCounterManifest(),
    startFrame: optimizedCounterStartFrame(),
    instantiateOptions: { userCaps: [["optimized-counter"]] },
  });
  const mounted = performance.now();
  const list = root.childNodes[0];
  for (let event = 0; event < EVENTS; event += 1) {
    root.dispatchEvent({
      type: "click",
      target: list,
      composedPath: () => [list, root],
      preventDefault() {},
    });
  }
  const completed = performance.now();
  assert.equal(list.childNodes[0].textContent, String(EVENTS));
  const runtime = app.getRuntimeStats();
  assert.equal(runtime.frames, EVENTS + 1);
  assert.equal(runtime.operations, 5 + EVENTS * 2);
  assert.equal(runtime.rootListeners, 1);
  assert.equal(runtime.activeEffects, 0);
  assert.equal(runtime.activeSubscriptions, 0);
  assert.ok(runtime.wasmMemoryPages <= 8);
  app.dispose();
  return {
    mountMs: mounted - mountStart,
    interactionMs: completed - mounted,
    transportBytes: runtime.outputBytes,
    wasmMemoryPages: runtime.wasmMemoryPages,
    domOperations: runtime.operations,
  };
}

const work = mkdtempSync(join(tmpdir(), "glamour-phase3-performance-"));
try {
  const wasmPath = join(work, "optimized-counter.wasm");
  execFileSync(
    BIN,
    ["compile", "src/optimized_counter.witchy", "--out", wasmPath],
    { cwd: PROJECT },
  );
  const wasmBytes = readFileSync(wasmPath);
  assert.ok(wasmBytes.byteLength <= 2 * 1024 * 1024);

  for (let warmup = 0; warmup < WARMUPS; warmup += 1) {
    await sampleReference(wasmBytes);
    await sampleOptimized(wasmBytes);
  }
  const reference = [];
  const optimized = [];
  for (let sample = 0; sample < SAMPLES; sample += 1) {
    reference.push(await sampleReference(wasmBytes));
    optimized.push(await sampleOptimized(wasmBytes));
  }

  const describe = (samples) => ({
    mountMs: summary(samples, "mountMs"),
    interactionMs: summary(samples, "interactionMs"),
    transportBytes: summary(samples, "transportBytes"),
    domOperations: summary(samples, "domOperations"),
    wasmMemoryPages: summary(samples, "wasmMemoryPages"),
  });
  const report = {
    schema: "witchy.glamour.phase3-performance.v1",
    commit: execFileSync("git", ["rev-parse", "HEAD"], {
      cwd: REPO,
      encoding: "utf8",
    }).trim(),
    dirty:
      execFileSync("git", ["status", "--porcelain"], {
        cwd: REPO,
        encoding: "utf8",
      }).trim().length > 0,
    host: {
      platform: process.platform,
      arch: process.arch,
      node: process.version,
    },
    workload: {
      name: "keyed-counter",
      events: EVENTS,
      samples: SAMPLES,
      warmups: WARMUPS,
      artifactBytes: wasmBytes.byteLength,
    },
    thresholds: {
      artifactBytesAtMost: 2 * 1024 * 1024,
      wasmMemoryPagesAtMost: 8,
      optimizedRootListeners: 1,
      optimizedInitialOperations: 5,
      optimizedOperationsPerInteraction: 2,
    },
    reference: describe(reference),
    optimized: describe(optimized),
  };
  process.stdout.write(`${JSON.stringify(report)}\nGLAMOUR-PHASE3-PERFORMANCE OK\n`);
} finally {
  rmSync(work, { recursive: true, force: true });
}

#!/usr/bin/env node
// RFC-0107 Phase 0: reproducible before-measurement for the current JSON host.
//
// Usage:
//   node web/witchy-runtime/glamour-baseline.mjs [path/to/witchy]

import { execFileSync } from "node:child_process";
import {
  copyFileSync,
  mkdtempSync,
  readFileSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import { mount } from "./glamour-dom.mjs";
import { createReferenceDom } from "./glamour-reference-dom.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const BIN = process.argv[2]
  ? resolve(process.cwd(), process.argv[2])
  : resolve(REPO, "target/debug/witchy");
const EVENTS = 100;
const work = mkdtempSync(join(tmpdir(), "glamour-baseline-"));

try {
  copyFileSync(
    join(REPO, "projects/glamour/src/glamour.witchy"),
    join(work, "glamour.witchy"),
  );
  copyFileSync(
    join(REPO, "projects/glamour/examples/counter/src/counter.witchy"),
    join(work, "counter.witchy"),
  );
  const wasmPath = join(work, "counter.wasm");
  execFileSync(BIN, ["compile", "counter.witchy", "--out", wasmPath], {
    cwd: work,
    stdio: "pipe",
  });
  const wasm = readFileSync(wasmPath);
  const dom = createReferenceDom();
  const root = dom.createRoot();
  const heapBefore = process.memoryUsage().heapUsed;
  const mountStart = performance.now();
  const app = await mount(wasm, root, {
    document: dom.document,
    initialModel: 0,
    instantiateOpts: { userCaps: [["counter"]] },
  });
  const mountMs = performance.now() - mountStart;
  const buttons = dom.findAll(root, "button");
  const plus = buttons.find((button) => button.textContent === "+");
  if (!plus) throw new Error("reference counter did not render its + button");

  const dispatchStart = performance.now();
  for (let index = 0; index < EVENTS; index++) {
    plus.dispatchEvent({ type: "click" });
  }
  const dispatchMs = performance.now() - dispatchStart;
  const runtime = app.getRuntimeStats();
  const heapAfter = process.memoryUsage().heapUsed;

  if (app.getModel() !== EVENTS) {
    throw new Error(`reference counter ended at ${app.getModel()}, expected ${EVENTS}`);
  }

  const report = {
    schema: "witchy.glamour.baseline.v1",
    workload: {
      name: "counter-increment",
      initialModel: 0,
      events: EVENTS,
      finalModel: app.getModel(),
    },
    host: {
      platform: process.platform,
      arch: process.arch,
      node: process.version,
    },
    artifact: {
      wasmBytes: wasm.byteLength,
    },
    timingMs: {
      mount: Number(mountMs.toFixed(3)),
      dispatchTotal: Number(dispatchMs.toFixed(3)),
      dispatchMean: Number((dispatchMs / EVENTS).toFixed(6)),
    },
    transport: runtime.protocol,
    memory: {
      wasmPages: runtime.wasmMemoryPages,
      jsHeapDeltaBytes: heapAfter - heapBefore,
    },
    domOperations: dom.snapshotOperations(),
  };
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
} finally {
  rmSync(work, { recursive: true, force: true });
}

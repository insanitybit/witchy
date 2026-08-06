#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { mountOptimized } from "./glamour-optimized.mjs";
import {
  optimizedCounterManifest,
  optimizedCounterResumeFrame,
  optimizedCounterResumeManifest,
  optimizedCounterStartFrame,
  optimizedCounterStaticOrder,
} from "./glamour-optimized-counter-fixture.mjs";
import { FakeElement, fakeDocument } from "./glamour-test-dom.mjs";

const BIN = resolve(process.argv[2] || "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const PROJECT = join(REPO, "projects/glamour/examples/optimized_counter");
const work = mkdtempSync(join(tmpdir(), "glamour-resume-differential-"));
const trigger = Object.freeze({
  plan: 31,
  node: 20,
  name: "click",
  value: "",
  checked: false,
  key: "",
  composing: false,
  userActivation: true,
});

function staticCounter(count) {
  const root = new FakeElement("root");
  const list = new FakeElement("ul");
  list.appendChild(fakeDocument.createTextNode(String(count)));
  for (const value of optimizedCounterStaticOrder(count)) {
    const item = new FakeElement("li");
    item.appendChild(fakeDocument.createTextNode(String(value)));
    list.appendChild(item);
  }
  root.appendChild(list);
  return root;
}

function semanticNode(node) {
  if (node instanceof FakeElement) {
    return {
      tag: node.tag,
      attributes: [...node.attributes].sort(([left], [right]) => left.localeCompare(right)),
      children: node.childNodes.map(semanticNode),
    };
  }
  return { text: node.textContent };
}

try {
  const wasmPath = join(work, "optimized-counter.wasm");
  execFileSync(
    BIN,
    ["compile", "src/optimized_counter.witchy", "--out", wasmPath],
    { cwd: PROJECT },
  );
  const wasmBytes = readFileSync(wasmPath);

  const freshRoot = new FakeElement("root");
  const fresh = await mountOptimized(wasmBytes, freshRoot, {
    document: fakeDocument,
    manifest: optimizedCounterManifest(),
    startFrame: optimizedCounterStartFrame(),
    instantiateOptions: { userCaps: [["fresh-counter"]] },
  });
  for (let count = 0; count < 6; count += 1) fresh.dispatch(trigger);

  const resumedRoot = staticCounter(5);
  const resumed = await mountOptimized(wasmBytes, resumedRoot, {
    document: fakeDocument,
    manifest: optimizedCounterResumeManifest(5),
    startFrame: optimizedCounterResumeFrame(5),
    resume: true,
    instantiateOptions: { userCaps: [["resumed-counter"]] },
  });
  resumed.dispatch(trigger);

  assert.deepEqual(
    semanticNode(resumedRoot),
    semanticNode(freshRoot),
    "resuming static state then dispatching is equivalent to the fresh trace",
  );
  assert.equal(resumed.getRuntimeStats().frames, 1, "resume applies no initial frame");
  assert.equal(fresh.getRuntimeStats().frames, 7);
  fresh.dispose();
  resumed.dispose();
  console.log("GLAMOUR-RESUME-DIFFERENTIAL OK");
} finally {
  rmSync(work, { recursive: true, force: true });
}

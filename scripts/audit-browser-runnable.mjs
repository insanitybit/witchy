#!/usr/bin/env node
// Audit: run EVERY book example the manifest marks `runnable: true` through the
// REAL browser path (`runWitchy` in web/witchy-host.js, driving web/witchy.wasm +
// the browser shim). A `runnable: true` example that fails to instantiate/run
// here is a false Run button — the exact class of bug the user hit with
// `vm.par_map` (Console-only footprint, but its lowering emits host imports the
// browser shim does not provide, so the module cannot instantiate).
//
// Usage:  node scripts/audit-browser-runnable.mjs
// Exit 0 iff every runnable example actually runs in the browser path.

import { runWitchy } from "../web/witchy-host.js";
import { readFileSync } from "node:fs";
import { dirname, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "..");

// Load the browser compiler wasm exports (same module the playground loads).
const wasmBytes = readFileSync(join(REPO, "web/witchy.wasm"));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const wasm = instance.exports;

// The manifest is the single source of truth for which blocks are runnable.
const manifest = JSON.parse(readFileSync(join(REPO, "book/examples.json"), "utf8"));

// Re-extract each file's fenced ```witchy blocks in order, so manifest `block`
// N maps to the Nth witchy fence in that file. (Mirrors the classifier's own
// block numbering.)
function witchyBlocks(mdPath) {
  const text = readFileSync(join(REPO, mdPath), "utf8");
  const lines = text.split("\n");
  const blocks = [];
  let cur = null;
  // Match the Rust classifier's extract_witchy_blocks EXACTLY: trim_end only
  // (not both sides), so block numbering aligns with book/examples.json.
  for (const line of lines) {
    if (cur === null && line.trimEnd() === "```witchy") { cur = []; continue; }
    if (cur !== null && line.trimEnd() === "```") { blocks.push(cur.join("\n")); cur = null; continue; }
    if (cur !== null) cur.push(line);
  }
  return blocks;
}

const byFile = new Map();
let failures = [];
let ran = 0;

for (const e of manifest) {
  if (!e.runnable) continue;
  if (!e.file.endsWith(".md")) continue;
  if (!byFile.has(e.file)) byFile.set(e.file, witchyBlocks(e.file));
  const blocks = byFile.get(e.file);
  const src = blocks[e.block - 1];
  if (src === undefined) {
    failures.push({ file: e.file, block: e.block, why: "manifest block index out of range (extraction drift)" });
    continue;
  }
  ran++;
  const r = await runWitchy(wasm, src);
  if (!r.ok) {
    failures.push({ file: e.file, block: e.block, footprint: e.footprint, why: r.text.split("\n")[0].slice(0, 120) });
  }
}

console.log(`ran ${ran} runnable book examples through the browser path`);
if (failures.length === 0) {
  console.log("ALL PASS — every runnable example instantiates + runs in the browser.");
  process.exit(0);
}
console.log(`\n${failures.length} FALSE-RUNNABLE (marked runnable but fail in-browser):`);
for (const f of failures) {
  console.log(`  ${f.file} block ${f.block}  [${(f.footprint || []).join(",")}]`);
  console.log(`    -> ${f.why}`);
}
process.exit(1);

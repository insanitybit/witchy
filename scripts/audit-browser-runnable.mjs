#!/usr/bin/env node
// Audit every complete, non-negative book example through the real browser
// path and the exact deterministic providers used by the live docs page.
// A complete example that is not classified browser-runnable, or a Run button
// that fails to compile/instantiate/run, is a gate failure.
//
// Usage:  node scripts/audit-browser-runnable.mjs
// Exit 0 iff every complete book example actually runs in the browser path.

import { runWitchy } from "../web/witchy-host.js";
import { DOCS_RUN_OPTIONS } from "../web/docs-run-options.js";
import { readFileSync } from "node:fs";
import { dirname, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "..");

// Load the browser compiler wasm exports (same module the playground loads).
const compilerPath =
  process.env.WITCHY_WASM_PATH || join(REPO, "web/witchy.wasm");
const wasmBytes = readFileSync(compilerPath);
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
const failures = [];
let ran = 0;
let complete = 0;

for (const e of manifest) {
  if (!e.file.startsWith("book/src/") || e.expect_error) continue;
  if (!byFile.has(e.file)) byFile.set(e.file, witchyBlocks(e.file));
  const blocks = byFile.get(e.file);
  const src = blocks[e.block - 1];
  if (src === undefined) {
    failures.push({ file: e.file, block: e.block, why: "manifest block index out of range (extraction drift)" });
    continue;
  }
  if (!/^\s*(?:pub\s+)?fn\s+main\s*\(/m.test(src)) continue;
  complete++;
  if (!e.browser_runnable) {
    failures.push({
      file: e.file,
      block: e.block,
      footprint: e.footprint,
      why: "complete example is not classified browser-runnable",
    });
    continue;
  }
  ran++;
  const r = await runWitchy(wasm, src, DOCS_RUN_OPTIONS);
  if (!r.ok) {
    failures.push({ file: e.file, block: e.block, footprint: e.footprint, why: r.text.split("\n")[0].slice(0, 120) });
  }
}

console.log(`ran ${ran}/${complete} complete book examples through the browser path`);
if (complete === 0) {
  console.log("VACUOUS: the manifest exposed no complete book examples");
  process.exit(1);
}
if (failures.length === 0) {
  console.log("ALL PASS — every complete book example instantiates + runs in the browser.");
  process.exit(0);
}
console.log(`\n${failures.length} COMPLETE BOOK EXAMPLE FAILURE(S):`);
for (const f of failures) {
  console.log(`  ${f.file} block ${f.block}  [${(f.footprint || []).join(",")}]`);
  console.log(`    -> ${f.why}`);
}
process.exit(1);

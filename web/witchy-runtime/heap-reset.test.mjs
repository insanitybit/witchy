#!/usr/bin/env node
// Regression test for the WASM string-export ABI's bump-allocator leak. A `String -> String`
// export (the glamour MVU run loop's per-event call) is PURE: every allocation it makes — the
// input header, all working memory, the result String — is dead once the host has read the
// result out. The `__galloc` bump allocator never frees, so without intervention each call
// leaks one call's worth of memory and a long-lived loop eventually exhausts WASM memory and
// `__galloc` returns an out-of-bounds pointer (the bug that crashed coven-web after a few dozen
// navigations). The fix: modules export their `__heap` pointer and the host resets it to its
// base after each call (witchy-runtime.mjs callString). This drives thousands of allocating
// calls and asserts memory stays BOUNDED and the heap returns to base every time.
//
// Usage:  node web/witchy-runtime/heap-reset.test.mjs [path/to/witchy-binary]

import { instantiate } from "./witchy-runtime.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

// A pure string export that ALLOCATES heavily per call: it grows the input 11x by concatenation,
// so each call churns the bump allocator (input + 10 growing intermediates + output).
const SRC = [
  "pub fn export_grow(s: String) -> String:",
  "    var out = s",
  "    for i in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]:",
  "        out = out + s",
  "    out",
  "",
].join("\n");

const work = mkdtempSync(join(tmpdir(), "heap-reset-"));
try {
  const srcPath = join(work, "grow.witchy");
  const wasmPath = join(work, "grow.wasm");
  writeFileSync(srcPath, SRC);
  execFileSync(BIN, ["compile", srcPath, "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  const rt = await instantiate(wasm);
  const heap = rt.instance.exports.__heap;
  ok(heap != null, "the module exports its `__heap` pointer (so the host can free between calls)");
  const base = heap.value;
  const bytesBefore = rt.memory.buffer.byteLength;

  const input = "abcdefghij".repeat(400); // 4000 chars; output is 11x = 44000 per call
  const wantLen = input.length * 11;
  let lengthsOk = true;
  let heapResetOk = true;
  const CALLS = 3000;
  for (let i = 0; i < CALLS; i++) {
    const out = rt.callString("__export_export_grow", input);
    if (out.length !== wantLen) lengthsOk = false;
    if (heap.value !== base) heapResetOk = false; // host restored the bump pointer after the call
  }
  const bytesAfter = rt.memory.buffer.byteLength;

  ok(lengthsOk, `all ${CALLS} calls returned the correct ${wantLen}-char result (no corruption)`);
  ok(heapResetOk, "the heap pointer is back at its base after every call");
  // The leak would have grown memory ~CALLS * (input+output) (≈130MB) or crashed with an
  // out-of-bounds `__galloc` long before 3000 calls. With the reset, memory plateaus at one
  // call's high-water mark — generously, well under 16MB.
  ok(bytesAfter <= 16 * 1024 * 1024, `memory stays bounded over ${CALLS} calls: ${(bytesAfter / 1024 / 1024).toFixed(1)}MB (was ${(bytesBefore / 1024 / 1024).toFixed(2)}MB)`);
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nHEAP-RESET FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nHEAP-RESET OK");
process.exit(0);

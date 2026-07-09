#!/usr/bin/env node
// (RFC-0045) The pure-compute shim carries an abort's MESSAGE, not a bare trap. It
// links the always-linked, authority-free `__witchy_abort` import and renders the
// shared DiagTemplate host-side, so a compiled abort throws a JS Error whose
// `.message` is the SAME `runtime error: <core>` text the wasmtime host and the
// interpreter produce. This compiles a small program per abort class, runs it via
// the shim's `run` export, and asserts the thrown message matches a committed
// oracle — twin-backend parity extended to the diagnostic surface.
//
// Usage:  node web/witchy-runtime/abort-message.test.mjs [path/to/witchy-binary]

import { instantiate } from "./witchy-runtime.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

// (program source, expected `runtime error: <core>` message the abort surfaces).
const CASES = [
  [
    'fn main(console: Console):\n    fail("the reason")\n',
    "runtime error: the reason",
  ],
  [
    'import list\nfn main(console: Console):\n    let xs = [1, 2]\n    console.print(__render(list.at(xs, 5)))\n',
    "runtime error: list index 5 out of bounds (length 2)",
  ],
  [
    'import string\nfn main(console: Console):\n    console.print(__render(string.to_int("junk")))\n',
    "runtime error: cannot parse `junk` as an Int",
  ],
  [
    'fn main(console: Console):\n    let nan = 0.0 / 0.0\n    console.print(__render(nan < 1.0))\n',
    "runtime error: cannot compare NaN",
  ],
];

const work = mkdtempSync(join(tmpdir(), "abort-message-"));
try {
  for (const [i, [src, want]] of CASES.entries()) {
    const srcPath = join(work, `abort${i}.witchy`);
    const wasmPath = join(work, `abort${i}.wasm`);
    writeFileSync(srcPath, src);
    execFileSync(BIN, ["compile", srcPath, "--out", wasmPath], { cwd: work });
    const wasm = readFileSync(wasmPath);

    const rt = await instantiate(wasm);
    let thrown = null;
    try {
      rt.run();
    } catch (e) {
      thrown = e;
    }
    ok(thrown !== null, `case ${i}: the abort throws (not a silent trap)`);
    ok(
      thrown && thrown.message === want,
      `case ${i}: message is the oracle \`${want}\`, got \`${thrown && thrown.message}\``,
    );
  }
} catch (e) {
  console.error("harness threw:", e);
  failures++;
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nABORT-MESSAGE FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nABORT-MESSAGE OK");
process.exit(0);

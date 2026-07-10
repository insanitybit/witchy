#!/usr/bin/env node
// (RFC-0045) The pure-compute shim carries an abort's MESSAGE, not a bare trap. It
// links the always-linked, authority-free `__witchy_abort` import and renders the
// shared DiagTemplate host-side, so a compiled abort throws a JS Error whose
// `.message` is the SAME location-prefixed `runtime error` text the native host
// and interpreter produce. This compiles a small program per abort class, runs it via
// the shim's `run` export, and asserts the thrown message matches a committed
// oracle — twin-backend parity extended to the diagnostic surface.
//
// Usage:  node web/witchy-runtime/abort-message.test.mjs [path/to/witchy-binary]
//         WITCHY_COMPILER_WASM=web/witchy.wasm node ...

import { instantiate } from "./witchy-runtime.mjs";
import { compile as compileInBrowser } from "../witchy-host.js";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");
const COMPILER_WASM = process.env.WITCHY_COMPILER_WASM;

let browserCompiler = null;
if (COMPILER_WASM) {
  const compilerBytes = readFileSync(resolve(COMPILER_WASM));
  const { instance } = await WebAssembly.instantiate(compilerBytes, {});
  browserCompiler = instance.exports;
}

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

// (program source, expected full runtime-error message the abort surfaces).
const CASES = [
  [
    'fn main(console: Console):\n    fail("the reason")\n',
    "runtime error: `main`, line 2: the reason",
  ],
  [
    'import list\nfn main(console: Console):\n    let xs = [1, 2]\n    console.print(__render(list.at(xs, 5)))\n',
    "runtime error: `main`, line 4: list index 5 out of bounds (length 2)",
  ],
  [
    'import string\nfn main(console: Console):\n    console.print(__render(string.to_int("junk")))\n',
    "runtime error: `main`, line 3: cannot parse `junk` as an Int",
  ],
  [
    'fn main(console: Console):\n    let nan = 0.0 / 0.0\n    console.print(__render(nan < 1.0))\n',
    "runtime error: `main`, line 3: cannot compare NaN",
  ],
  [
    'fn main(console: Console):\n    let z = 0\n    console.print(__render(10 / z))\n',
    "runtime error: `main`, line 3: division by zero",
  ],
  [
    'fn explode() -> Int:\n    let z = 0\n    10 / z\n\nfn main(console: Console):\n    console.print(__render(explode()))\n',
    "runtime error: `main.explode`, line 3: division by zero",
  ],
  [
    'import list\n\nfn probe() -> Int:\n    let inner = [7]\n    let _ = list.at(inner, 0)\n    9\n\nfn main(console: Console):\n    let outer = [1]\n    console.print(__render(list.at(outer, probe())))\n',
    "runtime error: `main`, line 10: list index 9 out of bounds (length 1)",
  ],
  [
    'fn make() -> fn() -> Int:\n    fn(): 10 / 0\n\nfn main(console: Console):\n    let explode = make()\n    console.print(__render(explode()))\n',
    "runtime error: `main.make`, line 2: division by zero",
  ],
  [
    'import list\n\nfn make_probe() -> fn() -> Int:\n    fn(): list.at([7], 0)\n\nfn main(console: Console):\n    let outer = [1]\n    let probe = make_probe()\n    console.print(__render(list.at(outer, probe())))\n',
    "runtime error: `main`, line 9: list index 7 out of bounds (length 1)",
  ],
  [
    'fn main(console: Console):\n    let z = 0\n    console.print(__render(10 % z))\n',
    "runtime error: `main`, line 3: modulo by zero",
  ],
  [
    'import bytes\nfn main(console: Console):\n    let b = bytes.from_string("a")\n    console.print(__render(bytes.at(b, 2)))\n',
    "runtime error: `main`, line 4: bytes index 2 out of bounds (length 1)",
  ],
  [
    'import math\nfn main(console: Console):\n    console.print(__render(math.to_int(0.0 / 0.0)))\n',
    "runtime error: `main`, line 3: math.to_int: NaN cannot be converted to Int",
  ],
  [
    'import dict\nfn main(console: Console):\n    let d: Dict(String, Int) = dict.new()\n    console.print(__render(dict.at(d, "missing")))\n',
    "runtime error: `main`, line 4: dict.at: missing key",
  ],
  [
    'fn main(console: Console):\n    let min = (0 - 9223372036854775807) - 1\n    console.print(__render(min / (0 - 1)))\n',
    "runtime error: `main`, line 3: integer overflow in `/`",
  ],
];

const work = mkdtempSync(join(tmpdir(), "abort-message-"));
try {
  for (const [i, [src, want]] of CASES.entries()) {
    // Keep the native compiler's module identity aligned with the source-only
    // browser compiler, whose synthetic module is named `main`.
    const srcPath = join(work, "main.witchy");
    const wasmPath = join(work, `abort${i}.wasm`);
    let wasm;
    if (browserCompiler) {
      wasm = compileInBrowser(browserCompiler, src);
    } else {
      writeFileSync(srcPath, src);
      execFileSync(BIN, ["compile", srcPath, "--out", wasmPath], { cwd: work });
      wasm = readFileSync(wasmPath);
    }

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

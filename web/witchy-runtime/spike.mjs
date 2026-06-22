#!/usr/bin/env node
// RFC-0007 spike: prove that a footprint-empty witchy rune, compiled to WASM,
// runs under the pure-compute JS host (witchy-runtime.mjs) and produces output
// IDENTICAL to the native interpreter run — and that a capability-using rune is
// structurally refused (deny-by-omission: missing-import LinkError).
//
// Usage:
//   node web/witchy-runtime/spike.mjs [path/to/witchy-binary]
// The binary defaults to ./target/debug/witchy (override for a release build).
//
// Exit 0 on success; non-zero (with a diff) on any mismatch.

import { instantiate } from "./witchy-runtime.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");

// A pure-compute rune: arithmetic, recursion, string concat + interpolation,
// and crypto.sha256. Its only host call is `print` (Console) — output, not
// authority — so its capability footprint is empty.
const PURE = `import crypto

fn fib(n: Int) -> Int:
    if n < 2:
        n
    else:
        fib(n - 1) + fib(n - 2)

fn main(console: Console):
    let sum = 2 + 3 * 4
    print(console, "sum = \${sum}\\n")
    print(console, "fib(15) = \${fib(15)}\\n")
    print(console, "hello, " + "browser" + "\\n")
    print(console, "sha256(abc) = \${crypto.sha256("abc")}\\n")
`;

// A capability-using rune: reads a file via a Dir capability. Non-empty
// footprint (Dir[Read]) -> imports a capability host fn the shim won't provide.
const IMPURE = `fn main(console: Console, root: Dir):
    print(console, read(root, "note.txt"))
`;

const work = mkdtempSync(join(tmpdir(), "witchy-rfc7-spike-"));
let failures = 0;
const ok = (cond, msg) => {
  if (cond) {
    console.log(`  ok: ${msg}`);
  } else {
    failures++;
    console.log(`  FAIL: ${msg}`);
  }
};

try {
  // --- 1. compile the pure rune to WASM ---
  const pureSrc = join(work, "pure.witchy");
  const pureWasm = join(work, "pure.wasm");
  writeFileSync(pureSrc, PURE);
  execFileSync(BIN, ["compile", pureSrc, "--out", pureWasm]);

  // --- 2. native interpreter run (the oracle) ---
  const native = execFileSync(BIN, [pureSrc], { encoding: "utf8" });
  // The native run prints each `print(...)` line; capture and normalize to the
  // shim's per-line list (the shim trims trailing newlines per call).
  const nativeLines = native.replace(/\n+$/, "").split("\n");

  // --- 3. run under the pure-compute shim ---
  const wasm = readFileSync(pureWasm);
  const { run } = await instantiate(wasm);
  const shimLines = run();

  console.log("native output:", JSON.stringify(nativeLines));
  console.log("shim   output:", JSON.stringify(shimLines));
  ok(
    JSON.stringify(shimLines) === JSON.stringify(nativeLines),
    "shim output matches the native interpreter byte-for-byte",
  );

  // --- 4. deny-by-omission: a capability rune cannot instantiate ---
  const impureSrc = join(work, "impure.witchy");
  const impureWasm = join(work, "impure.wasm");
  writeFileSync(impureSrc, IMPURE);
  execFileSync(BIN, ["compile", impureSrc, "--out", impureWasm]);
  let denied = false;
  let denyMsg = "";
  try {
    await instantiate(readFileSync(impureWasm));
    denyMsg = "(unexpectedly instantiated)";
  } catch (e) {
    denied = e instanceof WebAssembly.LinkError;
    denyMsg = `${e.constructor.name}: ${String(e.message).slice(0, 80)}`;
  }
  console.log("deny result:", denyMsg);
  ok(denied, "a capability-using rune is refused with a LinkError (deny-by-omission)");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nSPIKE FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nSPIKE OK");

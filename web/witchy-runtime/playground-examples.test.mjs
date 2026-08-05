#!/usr/bin/env node
// RFC-0041 P0: the standalone playground's example programs must be CURRENT witchy — the
// repair that made the playground work again (the old examples used removed forms like
// `int_to_string`, `<>`, `restrict`). This imports `EXAMPLES` straight from `web/playground.js`
// and, for each, compiles/runs it with the real toolchain: the five console examples must
// compile + run, and "Capabilities (a type error)" must FAIL to compile (that is its point —
// `write` on a `Dir[Read]` is a type error). So a future stale edit to the playground is caught.
//
// Usage:  node web/witchy-runtime/playground-examples.test.mjs [path/to/witchy-binary]

import { EXAMPLES } from "../playground.js";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

const work = mkdtempSync(join(tmpdir(), "pg-ex-"));
try {
  ok(Object.keys(EXAMPLES).length >= 5, `the playground ships several examples (${Object.keys(EXAMPLES).length})`);
  for (const [name, source] of Object.entries(EXAMPLES)) {
    const f = join(work, "ex.witchy");
    writeFileSync(f, source);
    if (name.toLowerCase().includes("type error")) {
      // A deliberate compile error — `witchy compile` must reject it, producing no wasm.
      let compiled = true;
      try {
        execFileSync(BIN, ["compile", f, "--out", join(work, "ex.wasm")], { stdio: "pipe", cwd: work });
      } catch {
        compiled = false;
      }
      ok(!compiled, `"${name}" is a deliberate compile error (rejected by the type checker)`);
    } else {
      // Should compile + run and print something.
      let out = "";
      let ran = true;
      try {
        out = execFileSync(BIN, [f], { encoding: "utf8", cwd: work });
      } catch (e) {
        ran = false;
        out = String((e && (e.stderr || e.message)) || e);
      }
      ok(ran && out.trim().length > 0, `"${name}" compiles + runs -> ${JSON.stringify(out.split("\n")[0]).slice(0, 44)}`);
    }
  }
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nPLAYGROUND-EXAMPLES FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nPLAYGROUND-EXAMPLES OK");
process.exit(0);

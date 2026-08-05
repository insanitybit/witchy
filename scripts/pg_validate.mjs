// Validates the browser playground's codegen path WITHOUT a browser: it loads the
// very same web/witchy.wasm and the very same host shim the page uses (the shared
// web/witchy-host.js module — Node and Chrome share V8), then diffs its output
// against the interpreter oracle (`WITCHY_INTERP=1 witchy run`) for the examples.
//
//   ./scripts/build-playground.sh && node scripts/pg_validate.mjs examples/*.witchy
import { readFileSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { runWitchy } from "../web/witchy-host.js";

const bytes = readFileSync(new URL("../web/witchy.wasm", import.meta.url));
const { instance } = await WebAssembly.instantiate(bytes, {});
const wasm = instance.exports;

const examples = process.argv.slice(2);
let pass = 0,
  fail = 0;
const tmp = "/tmp/pg_validate.witchy";
for (const path of examples) {
  let source, oracle;
  try {
    source = readFileSync(path, "utf8");
    writeFileSync(tmp, source);
    oracle = execFileSync("./target/release/witchy", [tmp], {
      env: { ...process.env, WITCHY_INTERP: "1" },
      encoding: "utf8",
    }).replace(/\n$/, "");
  } catch {
    continue; // missing file, or not runnable on the oracle (argv/etc.) — skip
  }
  if (oracle.includes("compiled OK — it's a library")) continue;
  const r = await runWitchy(wasm, source);
  if (r.ok && r.text === oracle) {
    pass++;
  } else {
    fail++;
    console.log(
      `FAIL ${path}\n  oracle:   ${JSON.stringify(oracle).slice(0, 200)}\n  playground: ${JSON.stringify(r.text).slice(0, 200)}`,
    );
  }
}
console.log(`\nplayground vs interpreter: ${pass} agree, ${fail} diverge`);
// Vacuity guard (RFC-0058 §4): if no example actually validated (empty argv, or every
// one skipped), the check compared nothing — do not pass silently.
if (pass + fail === 0) {
  console.log("VACUOUS: zero examples validated — pass an example set (e.g. examples/*.witchy)");
  process.exit(1);
}
process.exit(fail === 0 ? 0 : 1);

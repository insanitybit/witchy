// RFC-0040 browser value round-trip: a cap-gated export
// `pub fn export_step(ui: UiRoot, input: String) -> String` receives a real,
// host-minted `UiRoot` in the browser — the JS host stages the app's `[user_caps]`
// grant via `user_cap_field_len`, the `__export_step` wrapper mints the record, and
// the rune reads its policy. A missing grant traps (parity with the wasmtime host).
//
// Usage:  node web/witchy-runtime/user-cap-export.test.mjs [path/to/witchy-binary]

import { instantiate } from "./witchy-runtime.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");
const work = mkdtempSync(join(tmpdir(), "usercap-"));
let failed = false;
try {
  const src = [
    "grantable capability UiRoot:",
    "    policy: String",
    "",
    "pub fn export_step(ui: UiRoot, input: String) -> String:",
    "    match ui:",
    "        UiRoot(p) -> p + \":\" + input",
    "",
    "fn main(console: Console):",
    "    print(console, \"cli\")",
    "",
  ].join("\n");
  writeFileSync(join(work, "app.witchy"), src);
  const wasmPath = join(work, "app.wasm");
  execFileSync(BIN, ["compile", join(work, "app.witchy"), "--out", wasmPath], { stdio: "pipe" });
  const wasm = readFileSync(wasmPath);

  // With the grant staged, the minted UiRoot's policy reaches the rune.
  const { callString } = await instantiate(wasm, { userCaps: [["coven-web"]] });
  const out = callString("__export_export_step", "hi");
  if (out !== "coven-web:hi") {
    console.error(`FAIL: expected "coven-web:hi", got ${JSON.stringify(out)}`);
    failed = true;
  }

  // Without a grant, minting traps (both backends refuse identically).
  const { callString: cs2 } = await instantiate(wasm, {});
  let trapped = false;
  try {
    cs2("__export_export_step", "hi");
  } catch {
    trapped = true;
  }
  if (!trapped) {
    console.error("FAIL: a cap-gated export with no [user_caps] grant must trap");
    failed = true;
  }

  if (!failed) console.log("ok: RFC-0040 cap-gated export mints its UiRoot in the browser host");
} finally {
  rmSync(work, { recursive: true, force: true });
}
process.exit(failed ? 1 : 0);

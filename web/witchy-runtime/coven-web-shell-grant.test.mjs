#!/usr/bin/env node
// BUG-608 guard: the PRODUCTION coven-web host shell (projects/coven-web/web/src/main.ts)
// must keep declaring the `[user_caps]` grant the compiled coven_web_app rune requires at
// boot. The rune's `build_user_cap_field` traps at its first step
// (`user_cap_field_len — no [user_caps] grant`) unless mount is given
// `instantiateOpts: { userCaps: [["coven-web"]] }`. When main.ts drifted off that line
// (BUG-608), every from-source deploy mounted BLANK — yet the gate stayed green, because
// the existing glamour-coven-web-app.test.mjs mounts with its OWN opts, never the
// production shell's. This guard closes that hole: it (1) proves the grant is load-bearing
// by mounting the rune WITHOUT it and asserting the boot trap fires, then (2) extracts the
// grant the production main.ts actually passes to mount and (3) mounts the rune with THAT
// grant, asserting a real, non-empty render. Remove the userCaps line from main.ts and step
// 2's extraction fails; weaken it to the wrong grant and step 3's mount traps — either way
// this test (and the gate) goes red.
//
// Usage:  node web/witchy-runtime/coven-web-shell-grant.test.mjs [path/to/witchy-binary]

import { mount } from "./glamour-dom.mjs";
import { fakeDocument, FakeElement } from "./glamour-test-dom.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, copyFileSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const MAIN_TS = join(REPO, "projects/coven-web/web/src/main.ts");

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };
const settle = () => new Promise((r) => setTimeout(r, 0)).then(() => new Promise((r) => setTimeout(r, 0)));

// Strip TS comments before scanning so a stray `//`-commented mention can't be mistaken for
// the live grant (and, conversely, so the live grant is found even next to comments). Only
// treat `//` as a line comment when it is NOT preceded by `:` — leaves `https://` intact.
function stripComments(src) {
  return src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:])\/\/[^\n]*/g, "$1");
}

// Pull the `userCaps` grant that main.ts passes to `mount(..., { instantiateOpts: {...} })`.
// Returns the parsed value (e.g. [["coven-web"]]) or null if the shell declares none.
function extractUserCapsGrant(src) {
  const m = stripComments(src).match(
    /instantiateOpts\s*:\s*\{\s*userCaps\s*:\s*(\[\s*\[[^\]]*\]\s*\])/,
  );
  if (!m) return null;
  try { return JSON.parse(m[1]); } catch { return null; }
}

const work = mkdtempSync(join(tmpdir(), "coven-web-grant-"));
try {
  // Compile the SAME rune the production bundle inlines (build.sh step [4/6]).
  for (const f of ["src/glamour.witchy", "src/markdown.witchy", "src/highlight.witchy"]) {
    copyFileSync(join(REPO, "projects/glamour", f), join(work, f.replace("src/", "")));
  }
  copyFileSync(
    join(REPO, "projects/glamour/examples/coven_web_app/src/coven_web_app.witchy"),
    join(work, "coven_web_app.witchy"),
  );
  const wasmPath = join(work, "coven_web_app.wasm");
  execFileSync(BIN, ["compile", join(work, "coven_web_app.witchy"), "--out", wasmPath], { cwd: work });
  const wasm = readFileSync(wasmPath);

  const baseOpts = {
    document: fakeDocument,
    initialModel: { route: "/", session: "", data: "", notice: "", query: "" },
    routeTag: "Route",
    fetch: () => Promise.resolve({ status: 200, text: () => Promise.resolve(JSON.stringify({ runes: [] })) }),
    location: { pathname: "/" },
    history: { pushState: () => {} },
    ports: {},
  };

  // 1. The grant is load-bearing: mounting WITHOUT it must trip the boot trap. This is what
  //    a from-source deploy did when main.ts had drifted (the app mounted blank).
  let trapped = "";
  try {
    await mount(wasm, new FakeElement("root"), baseOpts); // no instantiateOpts -> no userCaps
    await settle();
  } catch (e) {
    trapped = e instanceof Error ? e.message : String(e);
  }
  ok(trapped.includes("user_cap_field_len"), "mounting the rune WITHOUT a userCaps grant trips the boot trap (grant is load-bearing)");

  // 2. The production shell still declares the grant the rune needs.
  const grant = extractUserCapsGrant(readFileSync(MAIN_TS, "utf8"));
  ok(grant !== null, "main.ts passes an instantiateOpts.userCaps grant to mount()");
  ok(JSON.stringify(grant) === JSON.stringify([["coven-web"]]),
    `main.ts declares the coven-web grant (got ${JSON.stringify(grant)})`);

  // 3. Boot-smoke: mounting with EXACTLY the grant main.ts provides boots without trapping
  //    and renders a non-empty tree. (If main.ts's grant were wrong, this mount would trap.)
  const root = new FakeElement("root");
  let bootErr = "";
  try {
    await mount(wasm, root, { ...baseOpts, instantiateOpts: { userCaps: grant } });
    await settle();
  } catch (e) {
    bootErr = e instanceof Error ? e.message : String(e);
  }
  ok(bootErr === "", `the rune boots under main.ts's grant without trapping (${bootErr || "no error"})`);
  ok(root.textContent.trim().length > 0, "the booted rune renders a non-empty tree (not a blank mount)");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nCOVEN-WEB-SHELL-GRANT FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nCOVEN-WEB-SHELL-GRANT OK");
process.exit(0);

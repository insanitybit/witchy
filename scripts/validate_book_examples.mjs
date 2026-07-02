// Validates the RUNNABLE BOOK without a browser (RFC-0041 Phase 3): loads the shipped
// web/witchy.wasm + the shared web/witchy-host.js — the very engine a reader's Run button uses
// — and, for every `runnable` block in book/examples.json, runs it and asserts the output
// equals the manifest's recorded oracle output. This closes the no-silent-drift loop: an
// in-book run can never diverge from the toolchain that produced examples.json (which recorded
// the interpreter's output). Node and Chrome share V8, so this is the real path, headless.
//
//   ./scripts/build-playground.sh && node scripts/validate_book_examples.mjs
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { runWitchy } from "../web/witchy-host.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "..");

const bytes = readFileSync(resolve(REPO, "web/witchy.wasm"));
const { instance } = await WebAssembly.instantiate(bytes, {});
const wasm = instance.exports;

// Pull every ```` ```witchy ```` fenced block — matches the Rust `extract_witchy_blocks`
// (each line + a trailing newline), so `block N` here is the manifest's `block N`.
function extractWitchyBlocks(md) {
  const blocks = [];
  let cur = null;
  for (const line of md.split("\n")) {
    if (cur === null) {
      if (line.replace(/\s+$/, "") === "```witchy") cur = [];
    } else if (line.replace(/\s+$/, "") === "```") {
      blocks.push(cur.map((l) => l + "\n").join(""));
      cur = null;
    } else {
      cur.push(line);
    }
  }
  return blocks;
}

const manifest = JSON.parse(readFileSync(resolve(REPO, "book/examples.json"), "utf8"));
const fileBlocks = {};
const blocksFor = (file) =>
  (fileBlocks[file] ||= extractWitchyBlocks(readFileSync(resolve(REPO, file), "utf8")));

let pass = 0,
  fail = 0,
  skipped = 0;
for (const entry of manifest) {
  if (!entry.runnable) {
    skipped++;
    continue;
  }
  const source = blocksFor(entry.file)[entry.block - 1];
  if (source == null) {
    fail++;
    console.log(`FAIL ${entry.file} #${entry.block}: block not found in the file`);
    continue;
  }
  const r = await runWitchy(wasm, source);
  const expected = (entry.output || []).join("\n");
  if (r.ok && r.text === expected) {
    pass++;
  } else {
    fail++;
    console.log(
      `FAIL ${entry.file} #${entry.block}\n  manifest: ${JSON.stringify(expected).slice(0, 200)}\n  browser:  ${JSON.stringify(r.text).slice(0, 200)}`,
    );
  }
}
console.log(`\nbook runnable blocks: ${pass} agree with the manifest, ${fail} diverge, ${skipped} non-runnable`);
process.exit(fail === 0 ? 0 : 1);

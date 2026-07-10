import { readFileSync } from "node:fs";
import {
  instantiate,
  WITCHY_ABI_VERSION,
  WITCHY_BROWSER_IMPORTS,
} from "./witchy-runtime.mjs";

const wasmPath = process.argv[2];
const expected = (process.env.WITCHY_EXPECTED_IMPORTS || "")
  .split("\n")
  .filter(Boolean)
  .sort();
const expectedVersion = Number(process.env.WITCHY_EXPECTED_ABI_VERSION);

if (!Number.isInteger(expectedVersion) || WITCHY_ABI_VERSION !== expectedVersion) {
  throw new Error(
    `browser ABI version mismatch: compiler=${expectedVersion}, browser=${WITCHY_ABI_VERSION}`
  );
}

if (WITCHY_BROWSER_IMPORTS.join("\0") !== expected.join("\0")) {
  throw new Error(
    `browser ABI catalog mismatch\n` +
    `  compiler: ${expected.join(", ")}\n` +
    `  browser:  ${WITCHY_BROWSER_IMPORTS.join(", ")}`
  );
}

// Instantiation executes the runtime's independent object-key assertion, so
// this checks both the exported declaration and the functions actually linked.
await instantiate(readFileSync(wasmPath));
console.log("WITCHY-IMPORT-CATALOG OK");

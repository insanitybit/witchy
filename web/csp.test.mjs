import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { DOCS_RUN_OPTIONS } from "./docs-run-options.js";
import { deriveContentSecurityPolicy } from "./witchy-runtime/witchy-runtime.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const frameHash = createHash("sha256")
  .update(readFileSync(join(here, "witchy-cell-frame.js")))
  .digest("base64");

function emittedPolicy(file) {
  const html = readFileSync(join(here, file), "utf8");
  const match = html.match(
    /<meta\s+http-equiv="Content-Security-Policy"\s+content="([^"]+)"/,
  );
  assert.ok(match, `${file} must emit a CSP before loading page code`);
  return match[1];
}

const playground = deriveContentSecurityPolicy(undefined, {
  hostConnect: ["'self'"],
  styleSources: ["'unsafe-inline'"],
});
assert.equal(emittedPolicy("index.html"), playground);
assert.match(playground, /connect-src 'self'(?:;|$)/);
assert.doesNotMatch(playground, /https?:\/\//);

const docs = deriveContentSecurityPolicy(DOCS_RUN_OPTIONS.capabilities, {
  hostConnect: ["'self'"],
  scriptSources: [
    "'self'",
    "'wasm-unsafe-eval'",
    "blob:",
    `'sha256-${frameHash}'`,
  ],
  imageSources: ["'self'", "data:"],
  fontSources: ["'self'"],
  frameSources: ["'self'"],
});
assert.equal(emittedPolicy("docs.html"), docs);
assert.match(docs, /connect-src 'self' https:\/\/example\.com:443(?:;|$)/);
assert.doesNotMatch(docs, /evil\.example/);
assert.match(docs, /frame-src 'self'(?:;|$)/);

const sealed = deriveContentSecurityPolicy(undefined, {
  scriptSources: ["'unsafe-inline'"],
  styleSources: ["'unsafe-inline'"],
});
assert.match(sealed, /connect-src 'none'(?:;|$)/);

assert.throws(
  () =>
    deriveContentSecurityPolicy(
      { fetch: { origins: ["https://safe.example; script-src *"] } },
      {},
    ),
  /invalid Fetch grant/,
);

console.log("csp: OK - emitted browser policies equal concrete capability surfaces");

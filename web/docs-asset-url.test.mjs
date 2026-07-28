#!/usr/bin/env node
// RFC-0041 regression: docs-bundle asset/content URLs must resolve against the BUNDLE ROOT,
// never the current client route. The bug this guards: from a chapter route `/p/<slug>`, a
// route-relative content URL resolved to `/p/content/...` (a 404), breaking every page after a
// nav. The FakeElement docs drivers can't reproduce it (no browser base-URL resolution), so this
// tests the pure resolution rule directly.
//
// Usage:  node web/docs-asset-url.test.mjs

import { assetUrl, contentUrl } from "./docs-asset-url.js";

let failures = 0;
const eq = (got, want, msg) => {
  const ok = String(got) === want;
  console.log(`  ${ok ? "ok" : "FAIL"}: ${msg}  (got ${got})`);
  if (!ok) failures++;
};

// The module lives at the bundle root, whatever that root is. The current PAGE (a chapter
// route) is irrelevant — resolution is against the module URL, so it never leaks the route.
for (const [label, base, page] of [
  ["root deploy", "http://localhost:8000/docs-boot.js", "http://localhost:8000/p/capabilities"],
  ["subpath deploy", "https://u.github.io/witchy/docs-boot.js", "https://u.github.io/witchy/p/capabilities-narrowing"],
]) {
  const root = new URL(".", base).href; // the bundle root
  eq(assetUrl("witchy.wasm", base).href, root + "witchy.wasm", `${label}: witchy.wasm at the bundle root`);
  eq(assetUrl("docs.wasm", base).href, root + "docs.wasm", `${label}: docs.wasm at the bundle root`);
  // The rune emits an ABSOLUTE `/content/<slug>.md`; it must land at the bundle root's content/,
  // NOT under the `${page}` route.
  eq(contentUrl("/content/capabilities.md", base).href, root + "content/capabilities.md", `${label}: /content resolves to the bundle root, not the route`);
  eq(contentUrl("/content/SUMMARY.md", base).href, root + "content/SUMMARY.md", `${label}: SUMMARY at the bundle root`);
  // Guard the exact regression: the resolved content URL must NOT contain the route segment.
  eq(String(contentUrl("/content/capabilities.md", base)).includes("/p/"), "false", `${label}: content URL never contains the /p/ route`);
}

// A non-absolute URL passes through untouched (defensive — the rune always sends absolute).
eq(contentUrl("already-relative.md", "http://h/docs-boot.js"), "already-relative.md", "non-absolute URL is passed through");

if (failures > 0) {
  console.error(`\nDOCS-ASSET-URL FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nDOCS-ASSET-URL OK");
process.exit(0);

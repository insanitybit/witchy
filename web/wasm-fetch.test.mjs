#!/usr/bin/env node
// The loud-failure contract of `web/wasm-fetch.js`: a wasm asset fetch that gets an
// HTML 404 / index-fallback page (the classic "expected magic word 00 61 73 6d,
// found 3c 21 44 4f") must reject with a message that names the URL, the status,
// and what to do — never leak the raw engine throw. Pure + fake-fetch driven: no
// browser, no server, no wasm build needed.
//
// Usage:  node web/wasm-fetch.test.mjs

import { looksLikeWasm, nonWasmDiagnosis, fetchWasm } from "./wasm-fetch.js";

let failures = 0;
const ok = (cond, msg) => { console.log(`  ${cond ? "ok" : "FAIL"}: ${msg}`); if (!cond) failures++; };

const WASM_MAGIC = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
const HTML_404 = new TextEncoder().encode("<!DOCTYPE HTML>\n<html><body>404 Not Found</body></html>");

// --- looksLikeWasm ---
ok(looksLikeWasm(WASM_MAGIC), "the wasm magic is recognized");
ok(!looksLikeWasm(HTML_404), "an HTML page is not wasm");
ok(!looksLikeWasm(new Uint8Array([])), "an empty body is not wasm");
ok(!looksLikeWasm(new Uint8Array([0x00, 0x61])), "a truncated magic is not wasm");

// --- nonWasmDiagnosis (pure) ---
ok(
  nonWasmDiagnosis({ url: "witchy.wasm", ok: true, status: 200, contentType: "application/wasm", bytes: WASM_MAGIC }) === null,
  "a genuine wasm response has no diagnosis",
);
const html404 = nonWasmDiagnosis({ url: "http://localhost:8000/witchy.wasm", ok: false, status: 404, contentType: "text/html;charset=utf-8", bytes: HTML_404 });
ok(html404 !== null, "an HTML 404 is diagnosed");
ok(html404.includes("http://localhost:8000/witchy.wasm"), "the diagnosis names the URL");
ok(html404.includes("HTTP 404"), "the diagnosis names the HTTP status");
ok(html404.includes("HTML"), "the diagnosis says the body was HTML, not wasm");
const spaFallback = nonWasmDiagnosis({ url: "witchy.wasm", ok: true, status: 200, contentType: "text/html", bytes: HTML_404 });
ok(spaFallback !== null && spaFallback.includes("HTML"), "a 200 SPA index fallback (HTML body) is still diagnosed");
const empty = nonWasmDiagnosis({ url: "witchy.wasm", ok: true, status: 200, contentType: "", bytes: new Uint8Array([]) });
ok(empty !== null && empty.includes("empty"), "an empty 200 body is diagnosed as empty");

// --- fetchWasm (fake fetch) ---
const fakeResponse = (status, contentType, bytes) => ({
  ok: status >= 200 && status < 300,
  status,
  headers: { get: (n) => (n.toLowerCase() === "content-type" ? contentType : null) },
  arrayBuffer: () => Promise.resolve(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength)),
});

const bytes = await fetchWasm("witchy.wasm", { fetch: () => Promise.resolve(fakeResponse(200, "application/wasm", WASM_MAGIC)) });
ok(bytes instanceof Uint8Array && looksLikeWasm(bytes), "a genuine wasm response resolves with the bytes");

let thrown = null;
try {
  await fetchWasm("witchy.wasm", {
    fetch: () => Promise.resolve(fakeResponse(404, "text/html;charset=utf-8", HTML_404)),
    hint: "run ./scripts/build-playground.sh first.",
  });
} catch (e) {
  thrown = e;
}
ok(thrown !== null, "an HTML 404 rejects");
ok(!!thrown && thrown.message.includes("HTTP 404"), "the rejection names the status");
ok(!!thrown && thrown.message.includes("run ./scripts/build-playground.sh first."), "the rejection carries the caller's hint");
ok(!!thrown && !thrown.message.includes("magic word 00 61 73 6d"), "the rejection is the diagnosis, not the raw engine throw");

if (failures > 0) {
  console.error(`\nWASM-FETCH FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nWASM-FETCH OK");
process.exit(0);

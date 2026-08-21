// Capability-denied worker shell. It authenticates the content-addressed Wasm
// bytes and grants the task only Witchy's non-authority String-to-String mechanics.

import { instantiate } from "./witchy-runtime.mjs";

const encoder = new TextEncoder();
const hex = (bytes) => [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
const exactKeys = (value, keys) => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  return actual.length === expected.length && actual.every((key, index) => key === expected[index]);
};

self.onmessage = async (event) => {
  const message = event?.data;
  if (!exactKeys(message, ["wasmUrl", "artifact", "exportName", "request", "maxRequestBytes", "maxResultBytes"])) {
    throw new Error("glamour worker shell: malformed request");
  }
  if (
    typeof message.wasmUrl !== "string" ||
    typeof message.artifact !== "string" || !/^glamour-worker1-[0-9a-f]{64}$/.test(message.artifact) ||
    message.exportName !== "__export_export_glamour_worker_execute" ||
    typeof message.request !== "string" ||
    !Number.isInteger(message.maxRequestBytes) || message.maxRequestBytes < 1 || message.maxRequestBytes > 65_536 ||
    !Number.isInteger(message.maxResultBytes) || message.maxResultBytes < 1 || message.maxResultBytes > 65_536 ||
    encoder.encode(message.request).byteLength > message.maxRequestBytes
  ) {
    throw new Error("glamour worker shell: request exceeds its authenticated contract");
  }
  const url = new URL(message.wasmUrl, self.location.href);
  if (url.origin !== self.location.origin || !/^worker-[0-9a-f]{16}\.wasm$/.test(url.pathname.split("/").pop() ?? "")) {
    throw new Error("glamour worker shell: executable URL is not same-origin content-addressed Wasm");
  }
  const response = await fetch(url, { credentials: "same-origin", redirect: "error" });
  if (!response.ok) throw new Error("glamour worker shell: executable fetch failed");
  const bytes = await response.arrayBuffer();
  if (bytes.byteLength === 0 || bytes.byteLength > 8 * 1024 * 1024) {
    throw new Error("glamour worker shell: executable size is invalid");
  }
  const digest = hex(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)));
  const fileDigest = url.pathname.split("/").pop().slice(7, 23);
  if (message.artifact !== `glamour-worker1-${digest}` || fileDigest !== digest.slice(0, 16)) {
    throw new Error("glamour worker shell: executable content identity differs from policy");
  }
  const runtime = await instantiate(bytes, {});
  const result = runtime.callString(message.exportName, message.request);
  if (typeof result !== "string" || encoder.encode(result).byteLength > message.maxResultBytes) {
    throw new Error("glamour worker shell: result exceeds its authenticated contract");
  }
  self.postMessage(Object.freeze({ result }));
  self.close();
};

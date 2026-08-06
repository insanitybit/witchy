// Fetch a `.wasm` asset and FAIL LOUDLY when the response is not WebAssembly.
//
// A missing bundle asset does not reject `fetch`: the server answers with an HTML
// 404 page (or an SPA index fallback), `arrayBuffer()` happily returns the HTML,
// and the user sees only the raw engine throw — `WebAssembly.instantiate():
// expected magic word 00 61 73 6d, found 3c 21 44 4f` (`<!DO…`). That is exactly
// how a bundle built without the browser compiler (`--allow-missing-compiler` /
// `just book`) failed. This helper checks the HTTP status AND the wasm magic and
// turns the failure into a message that says what happened and how to fix it.
//
// Pure check + thin fetch wrapper, so the check is unit-testable under Node
// (`web/wasm-fetch.test.mjs`) without a browser or a server.

/// Whether `bytes` starts with the WebAssembly magic (`\0asm`).
export function looksLikeWasm(bytes) {
  return bytes.length >= 4 && bytes[0] === 0x00 && bytes[1] === 0x61 && bytes[2] === 0x73 && bytes[3] === 0x6d;
}

/// The human diagnosis for a non-wasm response, or `null` when the response IS wasm.
/// Pure: takes the observed facts, returns the message (without the caller's hint).
export function nonWasmDiagnosis({ url, ok, status, contentType, bytes }) {
  if (ok && looksLikeWasm(bytes)) return null;
  const looksLikeHtml = bytes.length >= 1 && bytes[0] === 0x3c; // '<'
  const got = looksLikeHtml
    ? "an HTML page (a 404 or index fallback), not WebAssembly"
    : bytes.length === 0
      ? "an empty response"
      : "bytes without the wasm magic (\\0asm)";
  const statusPart = `HTTP ${status}${contentType ? ` (${contentType})` : ""}`;
  return `fetching ${url} returned ${statusPart} — the server sent ${got}. `
    + "The served directory is missing this wasm asset.";
}

/// Fetch `url`, verify the response is genuinely WebAssembly, and return the bytes
/// (a `Uint8Array`, accepted by `WebAssembly.instantiate`). On an HTTP error or a
/// non-wasm body, throw an `Error` whose message carries the diagnosis plus the
/// caller's `hint` (how to build/serve the asset). `opts.fetch` injects a fetch
/// for tests.
export async function fetchWasm(url, opts = {}) {
  const doFetch = opts.fetch || ((u) => fetch(u));
  const resp = await doFetch(url);
  const bytes = new Uint8Array(await resp.arrayBuffer());
  const diagnosis = nonWasmDiagnosis({
    url: String(url),
    ok: resp.ok,
    status: resp.status,
    contentType: (resp.headers && resp.headers.get && resp.headers.get("content-type")) || "",
    bytes,
  });
  if (diagnosis !== null) {
    throw new Error(opts.hint ? `${diagnosis} ${opts.hint}` : diagnosis);
  }
  return bytes;
}

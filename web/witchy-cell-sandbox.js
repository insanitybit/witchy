import { compile } from "./witchy-host.js";
import { deriveContentSecurityPolicy } from "./witchy-runtime/witchy-runtime.mjs";

const INIT = "witchy-cell-init-v1";
const PROGRESS = "witchy-cell-progress-v1";
const READY = "witchy-cell-ready-v1";
const RESULT = "witchy-cell-result-v1";
const DEFAULT_TIMEOUT_MS = 30_000;
const FRAME_SCRIPT_URL = new URL("./witchy-cell-frame.js", import.meta.url);

function escapeAttribute(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll('"', "&quot;")
    .replaceAll("<", "&lt;");
}

function randomNonce() {
  if (!globalThis.crypto || typeof globalThis.crypto.getRandomValues !== "function") {
    throw new Error("witchy sandbox: cryptographic randomness is unavailable");
  }
  const bytes = globalThis.crypto.getRandomValues(new Uint8Array(18));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

export function sandboxContentSecurityPolicy(
  capabilities,
  nonce,
) {
  return deriveContentSecurityPolicy(capabilities, {
    scriptSources: [`'nonce-${nonce}'`, "blob:", "'wasm-unsafe-eval'"],
    styleSources: [],
  });
}

export function sandboxSrcdoc(capabilities, nonce, frameSource) {
  if (typeof frameSource !== "string" || /<\/script/i.test(frameSource)) {
    throw new Error("witchy sandbox: invalid fixed frame bootstrap");
  }
  const policy = sandboxContentSecurityPolicy(capabilities, nonce);
  return `<!doctype html><meta charset="utf-8">`
    + `<meta http-equiv="Content-Security-Policy" content="${escapeAttribute(policy)}">`
    + `<script nonce="${escapeAttribute(nonce)}" `
    + `data-ready-token="${escapeAttribute(nonce)}">${frameSource}</script>`;
}

function assertPortable(value, path = "runOptions", seen = new Set()) {
  if (value === null || value === undefined) return;
  const kind = typeof value;
  if (kind === "string" || kind === "number" || kind === "boolean" || kind === "bigint") return;
  if (kind === "function" || kind === "symbol") {
    throw new Error(`witchy sandbox: ${path} is not structured-cloneable`);
  }
  if (kind !== "object") return;
  if (seen.has(value)) throw new Error(`witchy sandbox: ${path} is cyclic`);
  seen.add(value);
  if (Array.isArray(value)) {
    value.forEach((entry, index) => assertPortable(entry, `${path}[${index}]`, seen));
  } else if (value instanceof Uint8Array || value instanceof ArrayBuffer) {
    // Explicit byte fixtures are portable.
  } else {
    const proto = Object.getPrototypeOf(value);
    if (proto !== Object.prototype && proto !== null) {
      throw new Error(`witchy sandbox: ${path} must contain only plain data`);
    }
    for (const [name, entry] of Object.entries(value)) {
      assertPortable(entry, `${path}.${name}`, seen);
    }
  }
  seen.delete(value);
}

function removeFrame(frame) {
  if (typeof frame.remove === "function") frame.remove();
  else if (frame.parentNode && typeof frame.parentNode.removeChild === "function") {
    frame.parentNode.removeChild(frame);
  }
}

async function requestFrame({
  document: doc,
  capabilities,
  payload,
  frameSource,
  timeoutMs = DEFAULT_TIMEOUT_MS,
}) {
  if (!doc || !doc.body || typeof doc.createElement !== "function") {
    throw new Error("witchy sandbox: a live browser document is required");
  }
  if (typeof globalThis.MessageChannel !== "function") {
    throw new Error("witchy sandbox: MessageChannel is unavailable");
  }
  const view = doc.defaultView || globalThis;
  if (typeof view.addEventListener !== "function"
      || typeof view.removeEventListener !== "function") {
    throw new Error("witchy sandbox: a live browser window is required");
  }

  const nonce = randomNonce();
  const frame = doc.createElement("iframe");
  frame.setAttribute("sandbox", "allow-scripts");
  frame.setAttribute("hidden", "");
  frame.setAttribute("aria-hidden", "true");
  frame.setAttribute("tabindex", "-1");
  frame.srcdoc = sandboxSrcdoc(capabilities, nonce, frameSource);

  const channel = new MessageChannel();
  return new Promise((resolve, reject) => {
    let settled = false;
    let progress = "waiting for frame readiness";
    let onReady;
    const finish = (fn, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      view.removeEventListener("message", onReady);
      channel.port1.close();
      removeFrame(frame);
      fn(value);
    };
    const timer = setTimeout(
      () => finish(
        reject,
        new Error(`witchy sandbox: frame execution timed out (${progress})`),
      ),
      timeoutMs,
    );
    channel.port1.onmessage = (event) => {
      if (event.data && event.data.type === PROGRESS) {
        progress = event.data.stage;
        return;
      }
      if (!event.data || event.data.type !== RESULT) return;
      finish(resolve, event.data.result);
    };
    channel.port1.start();
    onReady = (event) => {
      if (event.source !== frame.contentWindow || !event.data
          || event.data.type !== READY || event.data.token !== nonce) return;
      view.removeEventListener("message", onReady);
      progress = "waiting for child initialization";
      try {
        frame.contentWindow.postMessage(
          { type: INIT, token: nonce, ...payload },
          "*",
          [channel.port2],
        );
      } catch (error) {
        finish(reject, error);
      }
    };
    view.addEventListener("message", onReady);
    doc.body.appendChild(frame);
  });
}

function loadText(url) {
  return fetch(url).then(async (response) => {
    if (!response.ok) {
      throw new Error(`witchy sandbox: failed to load ${url} (HTTP ${response.status})`);
    }
    return response.text();
  });
}

export function createSandboxedProgramRunner({
  document: doc = globalThis.document,
  loadCompiler,
  frameScriptUrl = FRAME_SCRIPT_URL,
  timeoutMs = DEFAULT_TIMEOUT_MS,
} = {}) {
  if (typeof loadCompiler !== "function") {
    throw new Error("witchy sandbox: loadCompiler must return { bytes, module, exports }");
  }
  let compilerPromise;
  let sourcesPromise;
  const compiler = () => (compilerPromise ||= Promise.resolve().then(loadCompiler));
  const sources = () => (sourcesPromise ||= Promise.all([
    loadText(frameScriptUrl),
    loadText(new URL("./witchy-host.js", import.meta.url)),
    loadText(new URL("./witchy-runtime/witchy-runtime.mjs", import.meta.url)),
  ]));

  return async (source, runOptions = {}) => {
    assertPortable(runOptions);
    let binary;
    let loaded;
    try {
      loaded = await compiler();
      if (!(loaded && loaded.module instanceof WebAssembly.Module && loaded.exports
          && (loaded.bytes instanceof Uint8Array || loaded.bytes instanceof ArrayBuffer))) {
        throw new Error("loadCompiler did not return { bytes, module, exports }");
      }
      binary = compile(loaded.exports, source);
    } catch (error) {
      return { ok: false, text: String((error && error.message) || error), stats: {} };
    }
    const [frameSource, hostSource, runtimeSource] = await sources();
    return requestFrame({
      document: doc,
      capabilities: runOptions.capabilities,
      frameSource,
      timeoutMs,
      payload: {
        action: "run",
        compilerBytes: loaded.bytes,
        binary,
        runOptions,
        hostSource,
        runtimeSource,
      },
    });
  };
}

export function probeSandboxFetch(
  url,
  capabilities,
  {
    document: doc = globalThis.document,
    frameScriptUrl = FRAME_SCRIPT_URL,
    frameSource,
    timeoutMs = DEFAULT_TIMEOUT_MS,
  } = {},
) {
  const source = frameSource === undefined ? loadText(frameScriptUrl) : Promise.resolve(frameSource);
  return source.then((loadedFrameSource) => requestFrame({
    document: doc,
    capabilities,
    frameSource: loadedFrameSource,
    timeoutMs,
    payload: { action: "probe-fetch", url: String(url) },
  }));
}

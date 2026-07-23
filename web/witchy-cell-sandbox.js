import { compile } from "./witchy-host.js";
import { deriveContentSecurityPolicy } from "./witchy-runtime/witchy-runtime.mjs";

const INIT = "witchy-cell-init-v1";
const RESULT = "witchy-cell-result-v1";
const DEFAULT_TIMEOUT_MS = 30_000;

// This is the only script admitted by each srcdoc frame. It accepts one private
// MessagePort from its parent, imports trusted source supplied over that port,
// and either runs one compiled guest or probes one Fetch URL. The frame has an
// opaque origin because its sandbox deliberately omits allow-same-origin.
const FRAME_BOOTSTRAP = `(() => {
  "use strict";
  let initialized = false;
  const trustedParent = parent;

  function materializeOptions(portable) {
    const options = { ...(portable || {}) };
    const fixture = options.fetchFixture;
    delete options.fetchFixture;
    if (fixture !== undefined) {
      if (!fixture || fixture.kind !== "text-prefix" || typeof fixture.prefix !== "string") {
        throw new Error("witchy sandbox: unsupported Fetch fixture");
      }
      options.fetchImpl = async (url) => {
        const bytes = new TextEncoder().encode(fixture.prefix + String(url));
        return {
          status: Number.isInteger(fixture.status) ? fixture.status : 200,
          redirected: false,
          type: "basic",
          headers: new Map([["content-type", "text/plain; charset=utf-8"]]),
          arrayBuffer: async () => bytes.buffer,
        };
      };
    }
    return options;
  }

  addEventListener("message", async (event) => {
    if (initialized || event.source !== trustedParent || !event.data
        || event.data.type !== "${INIT}" || event.ports.length !== 1) return;
    initialized = true;
    const port = event.ports[0];
    try {
      if (event.data.action === "probe-fetch") {
        const response = await fetch(event.data.url, { cache: "no-store", mode: "cors" });
        await response.arrayBuffer();
        port.postMessage({ type: "${RESULT}", result: { ok: true, status: response.status } });
        return;
      }
      if (event.data.action !== "run") throw new Error("unknown sandbox action");

      const runtimeUrl = "data:text/javascript;charset=utf-8,"
        + encodeURIComponent(event.data.runtimeSource);
      const expectedImport = 'from "./witchy-runtime/witchy-runtime.mjs";';
      if (!event.data.hostSource.includes(expectedImport)) {
        throw new Error("witchy sandbox: host/runtime import contract drifted");
      }
      const hostSource = event.data.hostSource.replace(
        expectedImport,
        "from " + JSON.stringify(runtimeUrl) + ";",
      );
      const hostUrl = "data:text/javascript;charset=utf-8," + encodeURIComponent(hostSource);
      const host = await import(hostUrl);
      const compiler = await WebAssembly.instantiate(event.data.compilerModule, {});
      const result = await host.runCompiledWitchy(
        compiler.exports,
        event.data.binary,
        materializeOptions(event.data.runOptions),
      );
      port.postMessage({ type: "${RESULT}", result });
    } catch (error) {
      port.postMessage({
        type: "${RESULT}",
        result: {
          ok: false,
          text: "sandbox error: " + String((error && error.message) || error),
          stats: {},
        },
      });
    }
  });
})();`;

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

export function sandboxContentSecurityPolicy(capabilities, nonce) {
  return deriveContentSecurityPolicy(capabilities, {
    scriptSources: [`'nonce-${nonce}'`, "data:", "'wasm-unsafe-eval'"],
    styleSources: [],
  });
}

export function sandboxSrcdoc(capabilities, nonce) {
  const policy = sandboxContentSecurityPolicy(capabilities, nonce);
  return `<!doctype html><meta charset="utf-8">`
    + `<meta http-equiv="Content-Security-Policy" content="${escapeAttribute(policy)}">`
    + `<script nonce="${escapeAttribute(nonce)}">${FRAME_BOOTSTRAP}</script>`;
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
  timeoutMs = DEFAULT_TIMEOUT_MS,
}) {
  if (!doc || !doc.body || typeof doc.createElement !== "function") {
    throw new Error("witchy sandbox: a live browser document is required");
  }
  if (typeof globalThis.MessageChannel !== "function") {
    throw new Error("witchy sandbox: MessageChannel is unavailable");
  }

  const nonce = randomNonce();
  const frame = doc.createElement("iframe");
  frame.setAttribute("sandbox", "allow-scripts");
  frame.setAttribute("hidden", "");
  frame.setAttribute("aria-hidden", "true");
  frame.setAttribute("tabindex", "-1");
  frame.srcdoc = sandboxSrcdoc(capabilities, nonce);

  const channel = new MessageChannel();
  return new Promise((resolve, reject) => {
    let settled = false;
    const finish = (fn, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      channel.port1.close();
      removeFrame(frame);
      fn(value);
    };
    const timer = setTimeout(
      () => finish(reject, new Error("witchy sandbox: frame execution timed out")),
      timeoutMs,
    );
    channel.port1.onmessage = (event) => {
      if (!event.data || event.data.type !== RESULT) return;
      finish(resolve, event.data.result);
    };
    channel.port1.start();
    frame.addEventListener("load", () => {
      try {
        frame.contentWindow.postMessage(
          { type: INIT, ...payload },
          "*",
          [channel.port2],
        );
      } catch (error) {
        finish(reject, error);
      }
    }, { once: true });
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
  hostSourceUrl = new URL("./witchy-host.js", import.meta.url),
  runtimeSourceUrl = new URL("./witchy-runtime/witchy-runtime.mjs", import.meta.url),
  timeoutMs = DEFAULT_TIMEOUT_MS,
} = {}) {
  if (typeof loadCompiler !== "function") {
    throw new Error("witchy sandbox: loadCompiler must return { module, exports }");
  }
  let compilerPromise;
  let sourcesPromise;
  const compiler = () => (compilerPromise ||= Promise.resolve().then(loadCompiler));
  const sources = () => (sourcesPromise ||= Promise.all([
    loadText(hostSourceUrl),
    loadText(runtimeSourceUrl),
  ]));

  return async (source, runOptions = {}) => {
    assertPortable(runOptions);
    let binary;
    let loaded;
    try {
      loaded = await compiler();
      if (!(loaded && loaded.module instanceof WebAssembly.Module && loaded.exports)) {
        throw new Error("loadCompiler did not return { module, exports }");
      }
      binary = compile(loaded.exports, source);
    } catch (error) {
      return { ok: false, text: String((error && error.message) || error), stats: {} };
    }
    const [hostSource, runtimeSource] = await sources();
    return requestFrame({
      document: doc,
      capabilities: runOptions.capabilities,
      timeoutMs,
      payload: {
        action: "run",
        compilerModule: loaded.module,
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
    timeoutMs = DEFAULT_TIMEOUT_MS,
  } = {},
) {
  return requestFrame({
    document: doc,
    capabilities,
    timeoutMs,
    payload: { action: "probe-fetch", url: String(url) },
  });
}

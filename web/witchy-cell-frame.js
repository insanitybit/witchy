(() => {
  "use strict";

  const INIT = "witchy-cell-init-v1";
  const PROGRESS = "witchy-cell-progress-v1";
  const READY = "witchy-cell-ready-v1";
  const RESULT = "witchy-cell-result-v1";
  const trustedParent = parent;
  const readyToken = document.currentScript.dataset.readyToken;
  let initialized = false;

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
    if (initialized || !event.data || event.data.type !== INIT
        || event.data.token !== readyToken || event.ports.length !== 1) return;
    initialized = true;
    const port = event.ports[0];
    try {
      port.postMessage({ type: PROGRESS, stage: "initialized" });
      if (event.data.action === "probe-fetch") {
        const response = await fetch(event.data.url, { cache: "no-store", mode: "cors" });
        await response.arrayBuffer();
        port.postMessage({ type: RESULT, result: { ok: true, status: response.status } });
        return;
      }
      if (event.data.action !== "run") throw new Error("unknown sandbox action");

      const runtimeUrl = URL.createObjectURL(new Blob(
        [event.data.runtimeSource],
        { type: "text/javascript" },
      ));
      let hostUrl;
      try {
        const expectedImport = 'from "./witchy-runtime/witchy-runtime.mjs";';
        if (!event.data.hostSource.includes(expectedImport)) {
          throw new Error("witchy sandbox: host/runtime import contract drifted");
        }
        const hostSource = event.data.hostSource.replace(
          expectedImport,
          `from ${JSON.stringify(runtimeUrl)};`,
        );
        hostUrl = URL.createObjectURL(new Blob([hostSource], { type: "text/javascript" }));
        port.postMessage({ type: PROGRESS, stage: "importing host runtime" });
        const host = await import(hostUrl);
        port.postMessage({ type: PROGRESS, stage: "instantiating compiler" });
        const { instance: compiler } = await WebAssembly.instantiate(
          event.data.compilerBytes,
          {},
        );
        port.postMessage({ type: PROGRESS, stage: "running guest" });
        const result = await host.runCompiledWitchy(
          compiler.exports,
          event.data.binary,
          materializeOptions(event.data.runOptions),
        );
        port.postMessage({ type: RESULT, result });
      } finally {
        if (hostUrl) URL.revokeObjectURL(hostUrl);
        URL.revokeObjectURL(runtimeUrl);
      }
    } catch (error) {
      port.postMessage({
        type: RESULT,
        result: {
          ok: false,
          text: `sandbox error: ${String((error && error.message) || error)}`,
          stats: {},
        },
      });
    }
  });

  trustedParent.postMessage({ type: READY, token: readyToken }, "*");
})();

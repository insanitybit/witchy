const FRAME_ID = /^glamour-frame1-[0-9a-f]{64}$/;
const FRAME_NONCE = /^glamour-frame-nonce1-[0-9a-f]{64}$/;
const FRAME_URL = /^\/assets\/frame-[0-9a-f]{16}\.html$/;
const encoder = new TextEncoder();

function fail(message) {
  throw new Error(`glamour frame: ${message}`);
}

function exact(value, keys) {
  return value && typeof value === "object" && !Array.isArray(value) &&
    Object.keys(value).sort().join("\0") === [...keys].sort().join("\0");
}

function attribute(node, name) {
  return typeof node?.getAttribute === "function" ? node.getAttribute(name) : null;
}

/** Own the private MessageChannel for compiler-authenticated opaque frames. */
export function installFrameCompartments(options = {}) {
  const records = options.frames;
  if (!Array.isArray(records)) fail("the frame registry must be an array");
  if (typeof options.resolveNode !== "function" || typeof options.dispatch !== "function") {
    fail("node resolution and typed dispatch are required");
  }
  const Channel = options.MessageChannel || globalThis.MessageChannel;
  if (records.length > 0 && typeof Channel !== "function") fail("MessageChannel is unavailable");
  const scopes = new Map();
  let disposed = false;

  const close = (scope) => {
    if (!scope || scope.closed) return;
    scope.closed = true;
    scope.node?.removeEventListener?.("load", scope.load);
    scope.port?.close?.();
    scope.port = null;
  };

  const report = (scope, error) => {
    close(scope);
    if (typeof options.onError === "function") options.onError(error);
  };

  const connect = (scope) => {
    if (disposed || scope.closed || scope.port || scope.node.contentWindow == null) return;
    const channel = new Channel();
    scope.port = channel.port1;
    scope.port.onmessage = (event) => {
      if (disposed || scope.closed) return;
      try {
        const message = event.data;
        if (exact(message, ["schema", "renderer", "nonce"]) && message.schema === "witchy.glamour.frame-ready.v1") {
          if (message.renderer !== scope.record.renderer || message.nonce !== scope.record.nonce || scope.ready) {
            fail("frame readiness does not match its authenticated instance");
          }
          scope.ready = true;
          scope.port.postMessage({
            schema: "witchy.glamour.frame-grant.v1",
            renderer: scope.record.renderer,
            nonce: scope.record.nonce,
            grant: scope.record.grant,
          });
          return;
        }
        if (!exact(message, ["schema", "renderer", "nonce", "value"]) ||
          message.schema !== "witchy.glamour.frame-event.v1" ||
          message.renderer !== scope.record.renderer || message.nonce !== scope.record.nonce ||
          typeof message.value !== "string" || encoder.encode(message.value).byteLength > scope.record.maxEventBytes ||
          !scope.ready) {
          fail("frame event does not match its authenticated channel");
        }
        options.dispatch({
          plan: scope.record.eventPlan,
          node: scope.record.node,
          name: "glamour-frame",
          value: message.value,
          checked: false,
          key: "",
          composing: false,
          userActivation: true,
        });
      } catch (error) {
        report(scope, error);
      }
    };
    scope.port.start?.();
    scope.node.contentWindow.postMessage({
      schema: "witchy.glamour.frame-init.v1",
      renderer: scope.record.renderer,
      nonce: scope.record.nonce,
    }, "*", [channel.port2]);
  };

  const validateRecord = (record) => {
    if (!exact(record, ["node", "eventPlan", "renderer", "maxGrantBytes", "maxEventBytes", "grant", "artifact", "url", "nonce"]) ||
      !Number.isInteger(record.node) || record.node <= 0 || !Number.isInteger(record.eventPlan) || record.eventPlan <= 0 ||
      record.renderer !== "document.v1" || record.maxGrantBytes !== 65_536 || record.maxEventBytes !== 4_096 ||
      typeof record.grant !== "string" || encoder.encode(record.grant).byteLength > record.maxGrantBytes ||
      typeof record.artifact !== "string" || !FRAME_ID.test(record.artifact) ||
      typeof record.url !== "string" || !FRAME_URL.test(record.url) ||
      typeof record.nonce !== "string" || !FRAME_NONCE.test(record.nonce)) {
      fail("frame registry entry is invalid");
    }
  };
  for (const record of records) validateRecord(record);

  const sync = () => {
    if (disposed) return;
    const live = new Set();
    for (const record of records) {
      const node = options.resolveNode(record.node);
      if (!node) continue;
      live.add(record.node);
      const existing = scopes.get(record.node);
      if (existing?.node === node && !existing.closed) continue;
      close(existing);
      if ((node.localName || node.tagName || "").toString().toLowerCase() !== "iframe" ||
        attribute(node, "sandbox") !== "allow-scripts") {
        fail(`frame node ${record.node} is not the authenticated sandbox`);
      }
      for (const [name, value] of [
        ["data-glamour-frame-renderer", record.renderer],
        ["data-glamour-frame-artifact", record.artifact],
        ["data-glamour-frame-nonce", record.nonce],
      ]) {
        const current = attribute(node, name);
        if (current != null && current !== value) fail(`frame node ${record.node} marker differs`);
        node.setAttribute(name, value);
      }
      const currentSrc = attribute(node, "src");
      if (currentSrc != null && currentSrc !== "" && currentSrc !== record.url) fail(`frame node ${record.node} source differs`);
      const scope = { record, node, port: null, ready: false, closed: false, load: null };
      scope.load = () => connect(scope);
      node.addEventListener?.("load", scope.load);
      scopes.set(record.node, scope);
      if (currentSrc !== record.url) node.setAttribute("src", record.url);
      else connect(scope);
    }
    for (const [node, scope] of scopes) {
      if (!live.has(node) || options.resolveNode(node) !== scope.node) {
        close(scope);
        scopes.delete(node);
      }
    }
  };

  return Object.freeze({
    sync,
    dispose() {
      if (disposed) return;
      disposed = true;
      for (const scope of scopes.values()) close(scope);
      scopes.clear();
    },
  });
}

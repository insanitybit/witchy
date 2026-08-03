// RFC-0107 resumable-island activation loader.
//
// This module is deliberately application-agnostic. It validates inert build
// records, owns activation scheduling, and loads an island artifact only after
// the declared trigger. A resumable artifact receives public state plus one
// allowlisted event snapshot. A fresh artifact receives no state and treats its
// first gesture only as activation; browser Event and DOM objects never cross
// into Wasm.

const INSTANCE_ID = /^glamour-instance1-[0-9a-f]{64}$/;
const ARTIFACT_ID = /^glamour-island1-[0-9a-f]{64}$/;
const WORKER_ID = /^glamour-worker1-[0-9a-f]{64}$/;
const FRAME_ID = /^glamour-frame1-[0-9a-f]{64}$/;
const FRAME_NONCE = /^glamour-frame-nonce1-[0-9a-f]{64}$/;
const BUILD_ID = /^[0-9a-f]{64}$/;
const KEY = /^[A-Za-z][A-Za-z0-9_-]*$/;
const EVENT = /^[a-z][a-z0-9_-]*$/;
const ACTIVATIONS = new Set(["load", "idle", "visible", "media", "interaction"]);
const PREFETCHES = new Set(["none", "idle", "visible", "media", "intent"]);
const MODES = new Set(["resume", "fresh"]);
const FRESH_INTERACTION_EVENTS = ["click", "change", "input", "keydown", "pointerdown", "submit", "focusin"];
const MAX_ISLANDS = 256;
const MAX_EVENTS_PER_ISLAND = 64;
const MAX_STATE_BYTES = 1024 * 1024;
const MAX_ARTIFACT_BYTES = 16 * 1024 * 1024;
const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });

function fail(message) {
  throw new Error(`glamour islands: ${message}`);
}

function ownRecord(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) fail(`${label} must be an object`);
  return value;
}

function exactKeys(value, allowed, label) {
  for (const key of Object.keys(value)) {
    if (!allowed.includes(key)) fail(`${label} contains unknown field ${key}`);
  }
}

function boundedString(value, label, maximum) {
  if (typeof value !== "string" || value.length === 0 || encoder.encode(value).byteLength > maximum) {
    fail(`${label} is invalid or oversized`);
  }
  return value;
}

function checkedMediaQuery(value, label) {
  const query = boundedString(value, label, 512);
  let depth = 0;
  for (const character of query) {
    if (character === "(") {
      depth += 1;
    } else if (character === ")") {
      depth -= 1;
      if (depth < 0) fail(`${label} is invalid`);
    } else if (!/[A-Za-z0-9 \t\n\r\-:./_%]/.test(character)) {
      fail(`${label} is invalid`);
    }
  }
  if (depth !== 0) fail(`${label} is invalid`);
  return query;
}

function natural(value, label) {
  if (!Number.isInteger(value) || value <= 0 || value > 0xffff_ffff) fail(`${label} must be a positive u32`);
  return value;
}

function checkedMountGrant(value) {
  const grant = ownRecord(value, "mount grant");
  exactKeys(grant, ["schema", "parameter", "capability", "policy", "digest"], "mount grant");
  if (
    grant.schema !== "witchy.web.ui-root-grant.v1" ||
    grant.capability !== "UiRoot" ||
    typeof grant.parameter !== "string" || !KEY.test(grant.parameter) ||
    typeof grant.policy !== "string" || grant.policy.length === 0 || encoder.encode(grant.policy).byteLength > 256 ||
    typeof grant.digest !== "string" || !BUILD_ID.test(grant.digest)
  ) {
    fail("mount grant is invalid");
  }
  return Object.freeze({ ...grant });
}

function checkedProgressiveUrl(value, label, allowEmpty = false) {
  if (allowEmpty && value === "") return value;
  const url = boundedString(value, label, 4096);
  const normalized = url.trim().toLowerCase();
  if (
    normalized.startsWith("javascript:") ||
    normalized.startsWith("vbscript:") ||
    normalized.startsWith("data:text/html") ||
    normalized.startsWith("//") ||
    /[\n\r\t]/.test(normalized)
  ) {
    fail(`${label} is unsafe`);
  }
  return url;
}

function checkedProgressiveFallback(value, island, eventName) {
  if (value == null) return null;
  const fallback = ownRecord(value, `island ${island} event fallback`);
  if (fallback.kind === "navigate") {
    exactKeys(fallback, ["kind", "href"], `island ${island} navigation fallback`);
    if (eventName !== "click") fail(`island ${island} navigation fallback requires click`);
    return Object.freeze({
      kind: "navigate",
      href: checkedProgressiveUrl(fallback.href, `island ${island} navigation fallback`),
    });
  }
  if (fallback.kind === "submit") {
    exactKeys(fallback, ["kind", "action", "method"], `island ${island} submission fallback`);
    if (eventName !== "click" && eventName !== "submit") {
      fail(`island ${island} submission fallback requires click or submit`);
    }
    if (fallback.method !== "get" && fallback.method !== "post") {
      fail(`island ${island} submission fallback method is invalid`);
    }
    return Object.freeze({
      kind: "submit",
      action: checkedProgressiveUrl(fallback.action, `island ${island} submission fallback`, true),
      method: fallback.method,
    });
  }
  fail(`island ${island} event fallback kind is invalid`);
}

function checkedEvent(value, island) {
  const event = ownRecord(value, `island ${island} event`);
  exactKeys(
    event,
    ["name", "node", "plan", "preventDefault", "stopPropagation", "readValue", "readChecked", "readKey", "fallback"],
    `island ${island} event`,
  );
  if (typeof event.name !== "string" || !EVENT.test(event.name)) {
    fail(`island ${island} has an invalid event name`);
  }
  for (const flag of ["preventDefault", "stopPropagation", "readValue", "readChecked", "readKey"]) {
    if (event[flag] !== undefined && typeof event[flag] !== "boolean") {
      fail(`island ${island} event ${event.name} has invalid ${flag}`);
    }
  }
  const fallback = checkedProgressiveFallback(event.fallback, island, event.name);
  if (fallback && event.preventDefault !== true) {
    fail(`island ${island} event ${event.name} has fallback without prevention`);
  }
  return Object.freeze({
    name: event.name,
    node: natural(event.node, `island ${island} event node`),
    plan: natural(event.plan, `island ${island} event plan`),
    preventDefault: event.preventDefault === true,
    stopPropagation: event.stopPropagation === true,
    readValue: event.readValue === true,
    readChecked: event.readChecked === true,
    readKey: event.readKey === true,
    fallback,
  });
}

function checkedIsland(value, buildIdentity) {
  const island = ownRecord(value, "island");
  exactKeys(
    island,
    ["id", "artifact", "key", "mode", "activation", "media", "prefetch", "prefetchMedia", "name", "state", "events", "grantDigest"],
    "island",
  );
  if (typeof island.id !== "string" || !INSTANCE_ID.test(island.id)) fail("island instance identity is invalid");
  if (typeof island.artifact !== "string" || !ARTIFACT_ID.test(island.artifact)) {
    fail(`island ${island.id} artifact identity is invalid`);
  }
  if (typeof island.key !== "string" || !KEY.test(island.key)) fail(`island ${island.id} key is invalid`);
  if (!MODES.has(island.mode)) fail(`island ${island.id} mode is invalid`);
  if (!ACTIVATIONS.has(island.activation)) fail(`island ${island.id} activation is invalid`);
  const media = island.activation === "media"
    ? checkedMediaQuery(island.media, `island ${island.id} media query`)
    : null;
  if (island.activation !== "media" && island.media != null) {
    fail(`island ${island.id} has media data without media activation`);
  }
  if (!PREFETCHES.has(island.prefetch)) fail(`island ${island.id} prefetch policy is invalid`);
  const prefetchMedia = island.prefetch === "media"
    ? checkedMediaQuery(island.prefetchMedia, `island ${island.id} prefetch media query`)
    : null;
  if (island.prefetch !== "media" && island.prefetchMedia != null) {
    fail(`island ${island.id} has media data without media prefetch`);
  }
  const name = island.name == null ? null : boundedString(island.name, `island ${island.id} diagnostic name`, 256);
  if (name != null && !KEY.test(name)) fail(`island ${island.id} diagnostic name is invalid`);
  let state = null;
  if (island.mode === "resume") {
    state = boundedString(island.state, `island ${island.id} public state`, MAX_STATE_BYTES);
    try {
      JSON.parse(state);
    } catch {
      fail(`island ${island.id} public state is not JSON`);
    }
  } else if (island.state !== null) {
    fail(`fresh island ${island.id} must not contain public state`);
  }
  if (!Array.isArray(island.events) || island.events.length > MAX_EVENTS_PER_ISLAND) {
    fail(`island ${island.id} event registry is invalid or oversized`);
  }
  const events = island.events.map((event) => checkedEvent(event, island.id));
  const pairs = new Set();
  for (const event of events) {
    const pair = `${event.name}:${event.node}`;
    if (pairs.has(pair)) fail(`island ${island.id} repeats event ${pair}`);
    pairs.add(pair);
  }
  if (island.mode === "resume" && island.activation === "interaction" && events.length === 0) {
    fail(`interaction island ${island.id} has no activating event`);
  }
  if (island.grantDigest !== undefined && (typeof island.grantDigest !== "string" || !BUILD_ID.test(island.grantDigest))) {
    fail(`island ${island.id} grant identity is invalid`);
  }
  return Object.freeze({
    id: island.id,
    artifact: island.artifact,
    key: island.key,
    mode: island.mode,
    buildIdentity,
    activation: island.activation,
    media,
    prefetch: island.prefetch,
    prefetchMedia,
    name,
    state,
    events: Object.freeze(events),
    grantDigest: island.grantDigest ?? null,
  });
}

function checkedManifest(value) {
  const manifest = ownRecord(value, "manifest");
  exactKeys(manifest, ["schema", "buildIdentity", "mountGrant", "islands"], "manifest");
  if (manifest.schema !== "witchy.glamour.islands.v1") fail("manifest schema is unsupported");
  if (typeof manifest.buildIdentity !== "string" || !BUILD_ID.test(manifest.buildIdentity)) {
    fail("manifest build identity is invalid");
  }
  if (!Array.isArray(manifest.islands) || manifest.islands.length > MAX_ISLANDS) {
    fail("manifest island registry is invalid or oversized");
  }
  const islands = manifest.islands.map((island) => checkedIsland(island, manifest.buildIdentity));
  const ids = new Set();
  const keys = new Set();
  for (const island of islands) {
    if (ids.has(island.id)) fail(`manifest repeats island identity ${island.id}`);
    if (keys.has(island.key)) fail(`manifest repeats island key ${island.key}`);
    ids.add(island.id);
    keys.add(island.key);
  }
  return Object.freeze({
    schema: manifest.schema,
    buildIdentity: manifest.buildIdentity,
    mountGrant: manifest.mountGrant === undefined ? null : checkedMountGrant(manifest.mountGrant),
    islands: Object.freeze(islands),
  });
}

function descriptorIds(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) fail(`${label} must be an object`);
  return Object.keys(value).map((key) => natural(Number(key), `${label} identity`)).sort((a, b) => a - b);
}

function checkedGrantIds(value, label) {
  if (!Array.isArray(value)) fail(`${label} must be an array`);
  const ids = value.map((id) => natural(id, `${label} identity`));
  if (ids.some((id, index) => index > 0 && id <= ids[index - 1])) fail(`${label} must be sorted and unique`);
  return ids;
}

function checkedDescriptorGrants(value, descriptors, browserPolicy, label) {
  const grants = ownRecord(value, label);
  const published = ownRecord(descriptors, `${label} descriptors`);
  const grantIds = descriptorIds(grants, label);
  const publishedIds = descriptorIds(published, `${label} descriptors`);
  if (JSON.stringify(grantIds) !== JSON.stringify(publishedIds)) fail(`${label} differs from its descriptors`);
  for (const id of grantIds) {
    const key = String(id);
    const grant = ownRecord(grants[key], `${label} ${id}`);
    const descriptor = ownRecord(published[key], `${label} descriptor ${id}`);
    exactKeys(grant, ["semantic", "policy"], `${label} ${id}`);
    exactKeys(descriptor, ["handler", "resultSchema", "completion", "ownerScope", "semantic", "policy"], `${label} descriptor ${id}`);
    if (grant.semantic !== descriptor.semantic || JSON.stringify(grant.policy) !== JSON.stringify(descriptor.policy)) {
      fail(`${label} ${id} differs from its descriptor`);
    }
    const policy = ownRecord(grant.policy, `${label} ${id} policy`);
    const admitted = (() => {
      if (grant.semantic === "timer" || grant.semantic === "interval") {
        exactKeys(policy, ["kind", "minimum"], `${label} ${id} policy`);
        return policy.kind === "timer" && browserPolicy.timers.some((entry) => entry.minimum === policy.minimum);
      }
      if (grant.semantic === "http") {
        exactKeys(policy, ["kind", "scope", "methods", "prefix"], `${label} ${id} policy`);
        return policy.kind === "fetch" && browserPolicy.fetch.some((entry) =>
          entry.scope === policy.scope && entry.prefix === policy.prefix &&
          JSON.stringify(entry.methods) === JSON.stringify(policy.methods));
      }
      if (grant.semantic === "navigation") {
        exactKeys(policy, ["kind", "base", "rights"], `${label} ${id} policy`);
        return policy.kind === "navigation" && browserPolicy.navigation.some((entry) =>
          entry.base === policy.base && entry.rights === policy.rights);
      }
      if (grant.semantic === "port" || grant.semantic === "secret") {
        exactKeys(policy, ["kind", "name"], `${label} ${id} policy`);
        return policy.kind === "port" && browserPolicy.ports.includes(policy.name);
      }
      if (grant.semantic === "host-port") {
        exactKeys(policy, ["kind", "adapter", "endpoint", "maxRequestBytes", "maxResultBytes"], `${label} ${id} policy`);
        return policy.kind === "host-port" &&
          new Set(["credential.get-exchange.v1", "credential.create-exchange.v1"]).has(policy.adapter) &&
          typeof policy.endpoint === "string" && /^\/(?!\/)[^\\?#\0]*$/.test(policy.endpoint) &&
          policy.maxRequestBytes === 61_440 && policy.maxResultBytes === 512 &&
          browserPolicy.ports.includes(policy.adapter);
      }
      if (new Set(["storage-get", "storage-set", "storage-remove"]).has(grant.semantic)) {
        exactKeys(policy, ["kind", "provider", "namespace", "keyPrefix", "maxValueBytes"], `${label} ${id} policy`);
        return policy.kind === "storage" && browserPolicy.storage.some((entry) =>
          entry.provider === policy.provider && entry.namespace === policy.namespace &&
          entry.keyPrefix === policy.keyPrefix && entry.maxValueBytes === policy.maxValueBytes);
      }
      if (grant.semantic === "worker") {
        exactKeys(policy, ["kind", "name", "maxRequestBytes", "maxResultBytes", "maxConcurrency", "timeoutMs", "artifact", "url", "export"], `${label} ${id} policy`);
        return policy.kind === "worker" && browserPolicy.workers.some((entry) =>
          JSON.stringify(entry) === JSON.stringify({
            name: policy.name,
            maxRequestBytes: policy.maxRequestBytes,
            maxResultBytes: policy.maxResultBytes,
            maxConcurrency: policy.maxConcurrency,
            timeoutMs: policy.timeoutMs,
            artifact: policy.artifact,
            url: policy.url,
            export: policy.export,
          }));
      }
      return false;
    })();
    if (!admitted) fail(`${label} ${id} policy is not admitted by the artifact projection`);
  }
  return Object.freeze(grants);
}

function checkedStaticControls(value, actions, browserPolicy, label) {
  const controls = ownRecord(value, label);
  exactKeys(controls, ["schema", "actions"], label);
  if (controls.schema !== "witchy.glamour.static-controls.v1") fail(`${label} is unsupported`);
  if (!Array.isArray(controls.actions) || controls.actions.length > 256) {
    fail(`${label} actions are invalid or oversized`);
  }
  if (JSON.stringify(controls.actions) !== JSON.stringify(actions)) {
    fail(`${label} differs from the published actions`);
  }
  const secretFields = [];
  const actionIds = new Set();
  for (const actionValue of controls.actions) {
    const action = ownRecord(actionValue, `${label} action`);
    exactKeys(action, ["id", "method", "action", "fields", "inputSchema", "resultSchema"], `${label} action`);
    if (typeof action.id !== "string" || !/^glamour-form1-[0-9a-f]{64}$/.test(action.id) || actionIds.has(action.id)) {
      fail(`${label} action identity is invalid or repeated`);
    }
    actionIds.add(action.id);
    if (action.method !== "GET" && action.method !== "POST") fail(`${label} action method is invalid`);
    checkedProgressiveUrl(action.action, `${label} action URL`, true);
    natural(action.inputSchema, `${label} action input schema`);
    natural(action.resultSchema, `${label} action result schema`);
    if (action.inputSchema === action.resultSchema) fail(`${label} action schemas collide`);
    if (!Array.isArray(action.fields) || action.fields.length > 256) fail(`${label} action fields are invalid`);
    const fieldNames = new Set();
    for (const fieldValue of action.fields) {
      const field = ownRecord(fieldValue, `${label} action field`);
      exactKeys(field, ["name", "label", "kind", "required"], `${label} action field`);
      if (typeof field.name !== "string" || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(field.name) || fieldNames.has(field.name)) {
        fail(`${label} action field identity is invalid or repeated`);
      }
      fieldNames.add(field.name);
      if (typeof field.label !== "string" || encoder.encode(field.label).byteLength > 1024) {
        fail(`${label} action field label is invalid or oversized`);
      }
      if (!new Set(["text", "email", "number", "checkbox", "secret"]).has(field.kind)) {
        fail(`${label} action field kind is invalid`);
      }
      if (typeof field.required !== "boolean") fail(`${label} action field required flag is invalid`);
      if (field.kind === "secret") {
        if (action.method !== "POST") fail(`${label} secret action requires POST`);
        secretFields.push(Object.freeze({ form: action.id, field: field.name }));
      }
    }
  }
  secretFields.sort((left, right) => {
    const leftKey = `${left.form}\0${left.field}`;
    const rightKey = `${right.form}\0${right.field}`;
    return leftKey < rightKey ? -1 : leftKey > rightKey ? 1 : 0;
  });
  const expectedSecrets = browserPolicy.secretFields;
  if (JSON.stringify(secretFields) !== JSON.stringify(expectedSecrets)) {
    fail(`${label} secret fields differ from the browser policy`);
  }
  return Object.freeze({ schema: controls.schema, actions: Object.freeze(controls.actions) });
}

function checkedBrowserPolicy(value, label) {
  const policy = ownRecord(value, label);
  exactKeys(policy, [
    "schema", "fetch", "navigation", "timers", "ports", "secretFields",
    "frames", "workers", "storage",
  ], label);
  if (policy.schema !== "witchy.glamour.browser-policy.v1") fail(`${label} is unsupported`);
  const list = (key) => {
    if (!Array.isArray(policy[key]) || policy[key].length > 128) fail(`${label} ${key} is invalid`);
    return policy[key];
  };
  const path = (value, field) => {
    const checked = boundedString(value, `${label} ${field}`, 2048);
    if (!checked.startsWith("/") || checked.startsWith("//") || /[\\?#\0]/.test(checked)) {
      fail(`${label} ${field} is not a same-origin absolute path`);
    }
    return checked;
  };
  const identifier = (value, field) => {
    const checked = boundedString(value, `${label} ${field}`, 128);
    if (!/^[A-Za-z][A-Za-z0-9_.-]*$/.test(checked)) fail(`${label} ${field} is invalid`);
    return checked;
  };
  const fetch = list("fetch").map((entry) => {
    const record = ownRecord(entry, `${label} Fetch entry`);
    exactKeys(record, ["scope", "methods", "prefix"], `${label} Fetch entry`);
    if (!Array.isArray(record.methods) || record.methods.length === 0) fail(`${label} Fetch methods are invalid`);
    const methods = record.methods.map((method) => {
      if (!new Set(["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"]).has(method)) {
        fail(`${label} Fetch method is invalid`);
      }
      return method;
    });
    if (methods.some((method, index) => index > 0 && method <= methods[index - 1])) {
      fail(`${label} Fetch methods must be sorted and unique`);
    }
    return Object.freeze({
      scope: identifier(record.scope, "Fetch scope"),
      methods: Object.freeze(methods),
      prefix: path(record.prefix, "Fetch prefix"),
    });
  });
  const navigation = list("navigation").map((entry) => {
    const record = ownRecord(entry, `${label} navigation entry`);
    exactKeys(record, ["base", "rights"], `${label} navigation entry`);
    if (record.rights !== "push" && record.rights !== "replace") fail(`${label} navigation rights are invalid`);
    return Object.freeze({ base: path(record.base, "navigation base"), rights: record.rights });
  });
  const timers = list("timers").map((entry) => {
    const record = ownRecord(entry, `${label} timer entry`);
    exactKeys(record, ["minimum"], `${label} timer entry`);
    if (!Number.isInteger(record.minimum) || record.minimum < 0 || record.minimum > 0x7fff_ffff) {
      fail(`${label} timer minimum is invalid`);
    }
    return Object.freeze({ minimum: record.minimum });
  });
  const ports = list("ports").map((port) => identifier(port, "port"));
  const secretFields = list("secretFields").map((entry) => {
    const record = ownRecord(entry, `${label} secret field entry`);
    exactKeys(record, ["form", "field"], `${label} secret field entry`);
    return Object.freeze({
      form: identifier(record.form, "secret form"),
      field: identifier(record.field, "secret field"),
    });
  });
  const storage = list("storage").map((entry) => {
    const record = ownRecord(entry, `${label} storage entry`);
    exactKeys(record, ["provider", "namespace", "keyPrefix", "maxValueBytes"], `${label} storage entry`);
    if (record.provider !== "session" && record.provider !== "local") fail(`${label} storage provider is invalid`);
    const keyPrefix = record.keyPrefix;
    if (typeof keyPrefix !== "string" || keyPrefix.includes("\0") || encoder.encode(keyPrefix).byteLength > 256) {
      fail(`${label} storage key prefix is invalid`);
    }
    if (!Number.isInteger(record.maxValueBytes) || record.maxValueBytes < 0 || record.maxValueBytes > 65_536) {
      fail(`${label} storage value limit is invalid`);
    }
    return Object.freeze({
      provider: record.provider,
      namespace: identifier(record.namespace, "storage namespace"),
      keyPrefix,
      maxValueBytes: record.maxValueBytes,
    });
  });
  const workers = list("workers").map((entry) => {
    const record = ownRecord(entry, `${label} worker entry`);
    exactKeys(record, ["name", "maxRequestBytes", "maxResultBytes", "maxConcurrency", "timeoutMs", "artifact", "url", "export"], `${label} worker entry`);
    if (
      !Number.isInteger(record.maxRequestBytes) || record.maxRequestBytes < 1 || record.maxRequestBytes > 65_536 ||
      !Number.isInteger(record.maxResultBytes) || record.maxResultBytes < 1 || record.maxResultBytes > 65_536 ||
      !Number.isInteger(record.maxConcurrency) || record.maxConcurrency < 1 || record.maxConcurrency > 16 ||
      !Number.isInteger(record.timeoutMs) || record.timeoutMs < 1 || record.timeoutMs > 300_000 ||
      typeof record.artifact !== "string" || !WORKER_ID.test(record.artifact) ||
      typeof record.url !== "string" || !/^\/assets\/worker-[0-9a-f]{16}\.wasm$/.test(record.url) ||
      record.export !== "__export_export_glamour_worker_execute"
    ) fail(`${label} worker entry is invalid`);
    return Object.freeze({
      name: identifier(record.name, "worker name"),
      maxRequestBytes: record.maxRequestBytes,
      maxResultBytes: record.maxResultBytes,
      maxConcurrency: record.maxConcurrency,
      timeoutMs: record.timeoutMs,
      artifact: record.artifact,
      url: record.url,
      export: record.export,
    });
  });
  const frames = list("frames").map((entry) => {
    const record = ownRecord(entry, `${label} frame entry`);
    exactKeys(record, ["renderer", "maxGrantBytes", "maxEventBytes", "artifact", "url"], `${label} frame entry`);
    if (
      record.renderer !== "document.v1" || record.maxGrantBytes !== 65_536 || record.maxEventBytes !== 4_096 ||
      typeof record.artifact !== "string" || !FRAME_ID.test(record.artifact) ||
      typeof record.url !== "string" || !/^\/assets\/frame-[0-9a-f]{16}\.html$/.test(record.url)
    ) fail(`${label} frame entry is invalid`);
    return Object.freeze({ ...record });
  });
  for (const [key, values] of [["fetch", fetch], ["navigation", navigation], ["timers", timers], ["ports", ports], ["secretFields", secretFields], ["frames", frames], ["workers", workers], ["storage", storage]]) {
    const keys = values.map((entry) => JSON.stringify(entry));
    if (keys.some((entry, index) => index > 0 && entry <= keys[index - 1])) {
      fail(`${label} ${key} must be sorted and unique`);
    }
  }
  return Object.freeze({
    schema: policy.schema,
    fetch: Object.freeze(fetch),
    navigation: Object.freeze(navigation),
    timers: Object.freeze(timers),
    ports: Object.freeze(ports),
    secretFields: Object.freeze(secretFields),
    frames: Object.freeze(frames),
    workers: Object.freeze(workers),
    storage: Object.freeze(storage),
  });
}

function checkedPublishedArtifact(value, publication, grant) {
  const artifact = ownRecord(value, "published artifact");
  exactKeys(artifact, [
    "artifact", "wireId", "registryId", "buildIdentity", "grantDigest", "grantProjection", "browserPolicy", "actions",
    "appId", "buildId", "features", "limits", "url", "moduleGroup", "programTypes",
    "templates", "nodes", "regions", "attributeBindings", "properties", "attributes", "aria",
    "ownerInstances",
    "customProperties", "eventClasses", "eventPlans", "effectDescriptors",
    "subscriptionDescriptors", "frames", "fresh", "resume",
  ], "published artifact");
  if (typeof artifact.artifact !== "string" || !ARTIFACT_ID.test(artifact.artifact)) fail("published artifact identity is invalid");
  if (artifact.buildIdentity !== publication.buildIdentity) fail(`artifact ${artifact.artifact} build identity differs`);
  if (artifact.grantDigest !== grant.digest) fail(`artifact ${artifact.artifact} grant digest differs`);
  if (artifact.buildId !== `0x${publication.buildIdentity.slice(0, 16)}`) fail(`artifact ${artifact.artifact} build ID differs`);
  if (typeof artifact.url !== "string" || !/^\/assets\/[A-Za-z0-9._-]+\.wasm$/.test(artifact.url)) {
    fail(`artifact ${artifact.artifact} URL is not a content asset`);
  }
  if (artifact.moduleGroup !== artifact.url.slice("/assets/".length)) fail(`artifact ${artifact.artifact} module group differs`);
  const projection = ownRecord(artifact.grantProjection, `artifact ${artifact.artifact} grant projection`);
  exactKeys(projection, ["schema", "projectGrantDigest", "effects", "subscriptions", "staticControls", "browserPolicy"], `artifact ${artifact.artifact} grant projection`);
  if (projection.schema !== "witchy.glamour.artifact-grant.v1") fail(`artifact ${artifact.artifact} grant projection is unsupported`);
  if (projection.projectGrantDigest !== grant.digest) fail(`artifact ${artifact.artifact} grant projection has the wrong project grant`);
  const browserPolicy = checkedBrowserPolicy(artifact.browserPolicy, `artifact ${artifact.artifact} browser policy`);
  const projectedBrowserPolicy = checkedBrowserPolicy(
    projection.browserPolicy,
    `artifact ${artifact.artifact} projected browser policy`,
  );
  if (JSON.stringify(projectedBrowserPolicy) !== JSON.stringify(browserPolicy)) {
    fail(`artifact ${artifact.artifact} browser policy differs from its grant projection`);
  }
  checkedDescriptorGrants(projection.effects, artifact.effectDescriptors, browserPolicy, `artifact ${artifact.artifact} effect grants`);
  checkedDescriptorGrants(projection.subscriptions, artifact.subscriptionDescriptors, browserPolicy, `artifact ${artifact.artifact} subscription grants`);
  if (!Array.isArray(artifact.frames) || artifact.frames.length > 128) fail(`artifact ${artifact.artifact} frame registry is invalid`);
  const frameNodes = new Set();
  const frames = artifact.frames.map((value) => {
    const frame = ownRecord(value, `artifact ${artifact.artifact} frame`);
    exactKeys(frame, ["node", "eventPlan", "renderer", "maxGrantBytes", "maxEventBytes", "grant", "artifact", "url", "nonce"], `artifact ${artifact.artifact} frame`);
    const policy = browserPolicy.frames.find((entry) => entry.renderer === frame.renderer && entry.artifact === frame.artifact);
    const eventPlan = artifact.eventPlans?.find?.((entry) => entry.id === frame.eventPlan);
    if (
      !Number.isInteger(frame.node) || frame.node <= 0 || frameNodes.has(frame.node) ||
      !Number.isInteger(frame.eventPlan) || frame.eventPlan <= 0 || eventPlan?.node !== frame.node || eventPlan?.readValue !== true ||
      frame.renderer !== "document.v1" || frame.maxGrantBytes !== 65_536 || frame.maxEventBytes !== 4_096 ||
      typeof frame.grant !== "string" || encoder.encode(frame.grant).byteLength > frame.maxGrantBytes ||
      typeof frame.artifact !== "string" || !FRAME_ID.test(frame.artifact) ||
      typeof frame.url !== "string" || !/^\/assets\/frame-[0-9a-f]{16}\.html$/.test(frame.url) ||
      typeof frame.nonce !== "string" || !FRAME_NONCE.test(frame.nonce) ||
      !policy || policy.url !== frame.url || policy.maxGrantBytes !== frame.maxGrantBytes || policy.maxEventBytes !== frame.maxEventBytes
    ) fail(`artifact ${artifact.artifact} frame is invalid`);
    frameNodes.add(frame.node);
    return Object.freeze({ ...frame });
  });
  const staticControls = checkedStaticControls(
    projection.staticControls,
    artifact.actions,
    browserPolicy,
    `artifact ${artifact.artifact} static controls`,
  );
  return Object.freeze({ ...artifact, browserPolicy, frames: Object.freeze(frames), actions: staticControls.actions });
}

function checkedArtifactPublication(value, manifest) {
  const publication = ownRecord(value, "artifact publication");
  exactKeys(publication, ["schema", "buildIdentity", "grantDigest", "artifacts", "workers", "frames"], "artifact publication");
  if (publication.schema !== "witchy.glamour.island-artifacts.v1") fail("artifact publication schema is unsupported");
  if (publication.buildIdentity !== manifest.buildIdentity) fail("artifact publication build identity differs");
  if (!manifest.mountGrant || publication.grantDigest !== manifest.mountGrant.digest) fail("artifact publication grant differs");
  if (!Array.isArray(publication.artifacts) || publication.artifacts.length > MAX_ISLANDS) fail("artifact publication registry is invalid or oversized");
  const artifacts = publication.artifacts.map((artifact) => checkedPublishedArtifact(artifact, publication, manifest.mountGrant));
  const workerRecords = publication.workers ?? [];
  if (!Array.isArray(workerRecords) || workerRecords.length > 128) fail("worker publication registry is invalid or oversized");
  const workers = workerRecords.map((value) => {
    const worker = ownRecord(value, "published worker");
    exactKeys(worker, ["artifact", "url", "export"], "published worker");
    if (
      typeof worker.artifact !== "string" || !WORKER_ID.test(worker.artifact) ||
      typeof worker.url !== "string" || !/^\/assets\/worker-[0-9a-f]{16}\.wasm$/.test(worker.url) ||
      worker.export !== "__export_export_glamour_worker_execute"
    ) fail("published worker is invalid");
    return Object.freeze(worker);
  });
  const workerByIdentity = new Map();
  for (const worker of workers) {
    if (workerByIdentity.has(worker.artifact)) fail(`worker publication repeats ${worker.artifact}`);
    workerByIdentity.set(worker.artifact, worker);
  }
  if (!Array.isArray(publication.frames) || publication.frames.length > 16) fail("frame publication registry is invalid or oversized");
  const frames = publication.frames.map((value) => {
    const frame = ownRecord(value, "published frame");
    exactKeys(frame, ["artifact", "url"], "published frame");
    if (typeof frame.artifact !== "string" || !FRAME_ID.test(frame.artifact) || typeof frame.url !== "string" || !/^\/assets\/frame-[0-9a-f]{16}\.html$/.test(frame.url)) {
      fail("published frame is invalid");
    }
    return Object.freeze(frame);
  });
  const frameByIdentity = new Map();
  for (const frame of frames) {
    if (frameByIdentity.has(frame.artifact)) fail(`frame publication repeats ${frame.artifact}`);
    frameByIdentity.set(frame.artifact, frame);
  }
  const byIdentity = new Map();
  for (const artifact of artifacts) {
    if (byIdentity.has(artifact.artifact)) fail(`artifact publication repeats ${artifact.artifact}`);
    byIdentity.set(artifact.artifact, artifact);
  }
  for (const island of manifest.islands) {
    if (!byIdentity.has(island.artifact)) fail(`island ${island.id} has no published artifact`);
  }
  for (const artifact of artifacts) {
    for (const worker of artifact.browserPolicy.workers) {
      const published = workerByIdentity.get(worker.artifact);
      if (!published || published.url !== worker.url || published.export !== worker.export) {
        fail(`artifact ${artifact.artifact} references an unpublished worker`);
      }
    }
    for (const frame of artifact.browserPolicy.frames) {
      const published = frameByIdentity.get(frame.artifact);
      if (!published || published.url !== frame.url) fail(`artifact ${artifact.artifact} references an unpublished frame`);
    }
  }
  return Object.freeze({ ...publication, artifacts: Object.freeze(artifacts), workers: Object.freeze(workers), frames: Object.freeze(frames), byIdentity, workerByIdentity, frameByIdentity });
}

function composedPath(event, root) {
  if (typeof event?.composedPath === "function") {
    const path = event.composedPath();
    const index = path.indexOf(root);
    return index >= 0 ? path.slice(0, index + 1) : [];
  }
  const path = [];
  let node = event?.target || null;
  while (node) {
    path.push(node);
    if (node === root) break;
    node = node.parentNode;
  }
  return path.at(-1) === root ? path : [];
}

function attribute(node, name) {
  return typeof node?.getAttribute === "function" ? node.getAttribute(name) : null;
}

function elementName(node) {
  const name = node?.localName || node?.tagName || node?.tag;
  return typeof name === "string" ? name.toLowerCase() : "";
}

function fallbackAttempt(event, matched, state) {
  const fallback = matched.plan.fallback;
  if (!fallback) return null;
  if (fallback.kind === "navigate") {
    return { fallback, node: matched.node, executed: false };
  }
  let form = matched.node;
  while (form && form !== state.element.parentNode && elementName(form) !== "form") {
    form = form.parentNode;
  }
  if (!form || elementName(form) !== "form") {
    fail(`island ${state.record.key} submission fallback has no form`);
  }
  return { fallback, node: form, submitter: event?.submitter || null, executed: false };
}

function checkedRedactedDevelopmentModel(application, island) {
  if (typeof application?.inspectDevelopment !== "function") return null;
  const inspection = ownRecord(
    application.inspectDevelopment(),
    `island ${island} development inspection`,
  );
  if (inspection.schema !== "witchy.glamour.devtools.v1") {
    fail(`island ${island} development inspection is unsupported`);
  }
  const model = ownRecord(inspection.model, `island ${island} development model`);
  exactKeys(model, ["schema", "snapshotFormat", "fields"], `island ${island} development model`);
  if (typeof model.schema !== "string" || !/^[0-9a-f]{64}$/.test(model.schema)) {
    fail(`island ${island} development model schema is invalid`);
  }
  if (![0, 1].includes(model.snapshotFormat) || !Array.isArray(model.fields) || model.fields.length > 256) {
    fail(`island ${island} development model fields are invalid or oversized`);
  }
  const names = new Set();
  const fields = model.fields.map((fieldValue, index) => {
    const field = ownRecord(fieldValue, `island ${island} development model field`);
    const keys = Object.hasOwn(field, "name")
      ? ["index", "name", "kind", "value"]
      : ["index", "kind", "value"];
    exactKeys(field, keys, `island ${island} development model field`);
    if (field.index !== index || !new Set(["Int", "Float", "Bool", "Aggregate"]).has(field.kind)) {
      fail(`island ${island} development model field is invalid`);
    }
    if (field.value !== "<redacted>") {
      fail(`island ${island} development model field exposed a value`);
    }
    if (Object.hasOwn(field, "name")) {
      if (
        typeof field.name !== "string" ||
        !/^[A-Za-z_][A-Za-z0-9_]*$/.test(field.name) ||
        encoder.encode(field.name).byteLength > 1024 ||
        names.has(field.name)
      ) {
        fail(`island ${island} development model field name is invalid or repeated`);
      }
      names.add(field.name);
    }
    return Object.freeze({
      index,
      ...(Object.hasOwn(field, "name") ? { name: field.name } : {}),
      kind: field.kind,
      value: "<redacted>",
    });
  });
  return Object.freeze({
    schema: model.schema,
    snapshotFormat: model.snapshotFormat,
    fields: Object.freeze(fields),
  });
}

function findEvent(path, state, eventName) {
  for (const node of path) {
    if (node === state.element.parentNode) break;
    const nodeText = attribute(node, "data-glamour-node");
    if (nodeText == null) continue;
    const nodeId = Number(nodeText);
    const plan = state.record.events.find((candidate) => candidate.name === eventName && candidate.node === nodeId);
    if (plan) return { node, plan };
    if (node === state.element) break;
  }
  return null;
}

function snapshotEvent(event, plan) {
  const target = event?.target;
  return Object.freeze({
    plan: plan.plan,
    node: plan.node,
    name: plan.name,
    value: plan.readValue && typeof target?.value === "string" ? target.value : "",
    checked: plan.readChecked && target?.checked === true,
    key: plan.readKey && typeof event?.key === "string" ? event.key : "",
    composing: event?.isComposing === true,
    userActivation: event?.isTrusted === true,
  });
}

export class IslandResumeMismatch extends Error {
  constructor(message = "resumable state does not match the artifact") {
    super(message);
    this.name = "IslandResumeMismatch";
  }
}

export class IslandDomMismatch extends IslandResumeMismatch {
  constructor(message = "authenticated static DOM does not match the artifact") {
    super(message);
    this.name = "IslandDomMismatch";
  }
}

/**
 * Install activation policy for one checked island manifest.
 *
 * `load(record)` is the first operation allowed to fetch or instantiate
 * application code. It returns `{identity, buildIdentity, resume}`. `resume`
 * receives the existing island element and public state, plus the activating
 * event snapshot exactly once. An authenticated DOM-adoption mismatch may throw
 * `IslandDomMismatch`, in which case `freshMount` rebuilds from the already
 * validated public state. Artifact, build, codec, and state mismatches fail.
 */
export function installIslands(options = {}) {
  const root = options.root || globalThis.document;
  if (!root || typeof root.addEventListener !== "function" || typeof root.querySelectorAll !== "function") {
    fail("root must support events and selector queries");
  }
  if (typeof options.load !== "function") fail("load must be a function");
  const manifest = checkedManifest(options.manifest);
  if (manifest.islands.some((island) => island.prefetch !== "none") && typeof options.prefetch !== "function") {
    fail("a manifest with prefetch policy requires a byte-only prefetch function");
  }
  const elements = [...root.querySelectorAll("[data-glamour-island]")];
  const byElementId = new Map();
  for (const element of elements) {
    const id = attribute(element, "data-glamour-island");
    if (!INSTANCE_ID.test(id || "") || byElementId.has(id)) fail("DOM contains an invalid or duplicate island identity");
    byElementId.set(id, element);
  }
  if (elements.length !== manifest.islands.length) fail("DOM and manifest island counts differ");

  const states = new Map();
  const statesById = new Map();
  for (const record of manifest.islands) {
    const element = byElementId.get(record.id);
    if (!element) fail(`DOM is missing island ${record.id}`);
    if (attribute(element, "data-glamour-build") !== manifest.buildIdentity) {
      fail(`island ${record.id} build identity does not match its manifest`);
    }
    const state = {
      record,
      element,
      status: "inert",
      mode: null,
      parent: null,
      application: null,
      queued: [],
      observer: null,
      media: null,
      idle: null,
      prefetchStatus: "not-requested",
      prefetchPending: null,
      prefetchObserver: null,
      prefetchMedia: null,
      prefetchMediaListener: null,
      prefetchIdle: null,
    };
    states.set(record.key, state);
    statesById.set(record.id, state);
  }
  for (const state of states.values()) {
    let parent = state.element.parentNode;
    while (parent && parent !== root) {
      const parentId = attribute(parent, "data-glamour-island");
      if (parentId != null) {
        const parentState = statesById.get(parentId);
        if (!parentState) fail(`island ${state.record.id} has an unknown parent island`);
        state.parent = parentState.record.id;
        break;
      }
      parent = parent.parentNode;
    }
  }

  let disposed = false;
  const listeners = new Map();
  const prefetchListeners = [];
  const nativeFallbackNodes = new WeakSet();
  const notify = (state, detail) => {
    if (typeof options.onState === "function") {
      options.onState(Object.freeze({ key: state.record.key, status: state.status, detail }));
    }
  };

  const executeProgressiveFallback = (state, attempt) => {
    const recovery = attempt?.fallback;
    if (!recovery || recovery.executed) return;
    recovery.executed = true;
    const { fallback, node } = recovery;
    if (fallback.kind === "navigate") {
      if (elementName(node) !== "a" || attribute(node, "href") !== fallback.href) {
        fail(`island ${state.record.key} navigation fallback DOM differs from its manifest`);
      }
      if (typeof options.navigate === "function") {
        options.navigate(fallback.href, node);
        return;
      }
      if (typeof node.click === "function") {
        nativeFallbackNodes.add(node);
        try {
          node.click();
        } finally {
          nativeFallbackNodes.delete(node);
        }
        return;
      }
      if (typeof globalThis.location?.assign === "function") {
        globalThis.location.assign(fallback.href);
        return;
      }
      fail(`island ${state.record.key} cannot execute its navigation fallback`);
    }
    const action = attribute(node, "action") ?? "";
    const method = (attribute(node, "method") ?? "get").toLowerCase();
    if (elementName(node) !== "form" || action !== fallback.action || method !== fallback.method) {
      fail(`island ${state.record.key} submission fallback DOM differs from its manifest`);
    }
    if (typeof options.submit === "function") {
      options.submit(node, recovery.submitter, Object.freeze({ action, method }));
      return;
    }
    nativeFallbackNodes.add(node);
    try {
      if (typeof node.requestSubmit === "function") {
        node.requestSubmit(recovery.submitter || undefined);
      } else if (typeof globalThis.HTMLFormElement?.prototype?.submit === "function") {
        globalThis.HTMLFormElement.prototype.submit.call(node);
      } else {
        fail(`island ${state.record.key} cannot execute its submission fallback`);
      }
    } finally {
      nativeFallbackNodes.delete(node);
    }
  };

  const dispatchQueued = async (state) => {
    if (state.queued.length === 0) return;
    if (typeof state.application?.dispatch !== "function") {
      state.queued.length = 0;
      fail(`active island ${state.record.key} cannot dispatch queued events`);
    }
    for (const attempt of state.queued.splice(0)) {
      await state.application.dispatch(attempt.trigger);
    }
  };

  const prefetchState = (state) => {
    if (disposed || state.status === "disposed" || state.record.prefetch === "none") return Promise.resolve(false);
    if (state.prefetchPending) return state.prefetchPending;
    state.prefetchStatus = "prefetching";
    state.prefetchPending = Promise.resolve()
      .then(() => options.prefetch(state.record))
      .then(() => {
        if (disposed || state.status === "disposed") return false;
        state.prefetchStatus = "prefetched";
        return true;
      })
      .catch((error) => {
        state.prefetchStatus = "failed";
        if (typeof options.onError === "function") options.onError(error, state.record);
        return false;
      });
    return state.prefetchPending;
  };

  const activateState = async (state, attempt = null) => {
    const trigger = attempt?.trigger ?? null;
    if (disposed) fail("loader is disposed");
    if (state.status === "active") {
      if (trigger) {
        if (typeof state.application?.dispatch !== "function") fail(`active island ${state.record.key} cannot dispatch events`);
        await state.application.dispatch(trigger);
      }
      return state.application;
    }
    if (state.status === "loading") {
      if (trigger) {
        if (state.queued.length >= MAX_EVENTS_PER_ISLAND) {
          fail(`island ${state.record.key} activation queue is full`);
        }
        state.queued.push(attempt);
      }
      return state.pending;
    }
    if (state.status === "failed") fail(`island ${state.record.key} activation previously failed`);
    state.observer?.disconnect();
    state.observer = null;
    if (state.idle != null) cancelIdle(state.idle);
    state.idle = null;
    if (state.mediaListener) state.media?.removeEventListener?.("change", state.mediaListener);
    state.mediaListener = null;
    state.status = "loading";
    notify(state);
    state.pending = (async () => {
      try {
        if (state.prefetchPending) await state.prefetchPending;
        let activationMode = state.record.mode;
        const artifact = ownRecord(await options.load(state.record), `island ${state.record.key} artifact`);
        if (disposed || state.status === "disposed") {
          artifact.dispose?.();
          fail(`island ${state.record.key} loader was disposed during activation`);
        }
        const identityMismatch =
          artifact.identity !== state.record.artifact || artifact.buildIdentity !== manifest.buildIdentity
            ? new IslandResumeMismatch("artifact identity does not match static island metadata")
            : null;
        if (identityMismatch) throw identityMismatch;
        if (state.record.mode === "fresh") {
          if (typeof artifact.fresh !== "function") fail(`island ${state.record.key} artifact has no fresh-start function`);
          state.application = await artifact.fresh(state.element, Object.freeze({ trigger: null }));
        } else {
          if (typeof artifact.resume !== "function") fail(`island ${state.record.key} artifact has no resume function`);
          try {
            state.application = await artifact.resume(state.element, Object.freeze({
              state: state.record.state,
              trigger,
            }));
          } catch (error) {
            if (!(error instanceof IslandDomMismatch) || typeof options.freshMount !== "function") throw error;
            activationMode = "fresh-from-public-state";
            state.application = await options.freshMount(state.element, state.record, artifact, trigger, error);
          }
        }
        if (disposed || state.status === "disposed") {
          state.application?.dispose?.();
          state.application = null;
          fail(`island ${state.record.key} loader was disposed during activation`);
        }
        if (!state.application || typeof state.application.dispose !== "function") {
          fail(`island ${state.record.key} activation returned no disposable application`);
        }
        state.mode = activationMode;
        state.status = "active";
        notify(state);
        await dispatchQueued(state);
        return state.application;
      } catch (error) {
        if (disposed || state.status === "disposed") {
          state.status = "disposed";
          throw error;
        }
        state.status = "failed";
        const failedAttempts = [attempt, ...state.queued.splice(0)];
        notify(state, error instanceof Error ? error.message : String(error));
        if (typeof options.onError === "function") options.onError(error, state.record);
        for (const failedAttempt of failedAttempts) {
          try {
            executeProgressiveFallback(state, failedAttempt);
          } catch (fallbackError) {
            if (typeof options.onError === "function") options.onError(fallbackError, state.record);
          }
        }
        throw error;
      }
    })();
    return state.pending;
  };

  const eventNames = new Set(
    manifest.islands
      .flatMap((island) => island.events.map((event) => event.name)),
  );
  if (manifest.islands.some((island) => island.mode === "fresh" && island.activation === "interaction")) {
    for (const name of FRESH_INTERACTION_EVENTS) eventNames.add(name);
  }
  for (const name of eventNames) {
    const listener = (event) => {
      if (disposed) return;
      const path = composedPath(event, root);
      if (path.some((node) => nativeFallbackNodes.has(node))) return;
      const islandElement = path.find((node) => attribute(node, "data-glamour-island") != null);
      if (!islandElement) return;
      const id = attribute(islandElement, "data-glamour-island");
      const state = statesById.get(id);
      if (!state) return;
      if (state.status === "active" && options.applicationOwnsEvents === true) return;
      const matched = findEvent(path, state, name);
      if (!matched) {
        if (state.record.mode !== "fresh" || state.record.activation !== "interaction" || state.status === "active") return;
        void activateState(state, null).catch((error) => {
          if (typeof options.onError === "function" && state.status === "active") {
            options.onError(error, state.record);
          }
        });
        return;
      }
      if (state.status === "loading" && state.queued.length >= MAX_EVENTS_PER_ISLAND) return;
      if (matched.plan.preventDefault && typeof event?.preventDefault === "function") event.preventDefault();
      if (matched.plan.stopPropagation && typeof event?.stopPropagation === "function") event.stopPropagation();
      const trigger = snapshotEvent(event, matched.plan);
      const attempt = Object.freeze({
        trigger,
        fallback: fallbackAttempt(event, matched, state),
      });
      void activateState(state, attempt).catch((error) => {
        if (typeof options.onError === "function" && state.status === "active") {
          options.onError(error, state.record);
        }
      });
    };
    root.addEventListener(name, listener, false);
    listeners.set(name, listener);
  }

  const queueMicrotaskImpl = options.queueMicrotask || globalThis.queueMicrotask || ((fn) => Promise.resolve().then(fn));
  const requestIdle = options.requestIdleCallback || globalThis.requestIdleCallback || ((fn) => setTimeout(fn, 1));
  const cancelIdle = options.cancelIdleCallback || globalThis.cancelIdleCallback || clearTimeout;
  const IntersectionObserverImpl = options.IntersectionObserver || globalThis.IntersectionObserver;
  const matchMediaImpl = options.matchMedia || globalThis.matchMedia;
  for (const state of states.values()) {
    switch (state.record.prefetch) {
      case "none":
        break;
      case "idle":
        state.prefetchIdle = requestIdle(() => void prefetchState(state));
        break;
      case "visible":
        if (typeof IntersectionObserverImpl !== "function") fail("visible prefetch requires IntersectionObserver");
        state.prefetchObserver = new IntersectionObserverImpl((entries) => {
          if (entries.some((entry) => entry.target === state.element && entry.isIntersecting)) {
            state.prefetchObserver?.disconnect();
            state.prefetchObserver = null;
            void prefetchState(state);
          }
        }, { rootMargin: "100% 0px" });
        state.prefetchObserver.observe(state.element);
        break;
      case "media":
        if (typeof matchMediaImpl !== "function") fail("media prefetch requires matchMedia");
        state.prefetchMedia = matchMediaImpl(state.record.prefetchMedia);
        if (state.prefetchMedia.matches) {
          queueMicrotaskImpl(() => void prefetchState(state));
        } else {
          const onChange = (event) => {
            if (!event.matches) return;
            state.prefetchMedia.removeEventListener?.("change", onChange);
            void prefetchState(state);
          };
          state.prefetchMedia.addEventListener?.("change", onChange, { once: true });
          state.prefetchMediaListener = onChange;
        }
        break;
      case "intent":
        break;
      default:
        fail(`unhandled prefetch ${state.record.prefetch}`);
    }
  }
  if (manifest.islands.some((island) => island.prefetch === "intent")) {
    for (const name of ["pointerover", "focusin"]) {
      const listener = (event) => {
        if (disposed) return;
        const path = composedPath(event, root);
        const islandElement = path.find((node) => attribute(node, "data-glamour-island") != null);
        if (!islandElement) return;
        const state = statesById.get(attribute(islandElement, "data-glamour-island"));
        if (state?.record.prefetch === "intent") void prefetchState(state);
      };
      root.addEventListener(name, listener, false);
      prefetchListeners.push([name, listener]);
    }
  }
  for (const state of states.values()) {
    switch (state.record.activation) {
      case "load":
        queueMicrotaskImpl(() => void activateState(state).catch(() => {}));
        break;
      case "idle":
        state.idle = requestIdle(() => void activateState(state).catch(() => {}));
        break;
      case "visible":
        if (typeof IntersectionObserverImpl !== "function") fail("visible activation requires IntersectionObserver");
        state.observer = new IntersectionObserverImpl((entries) => {
          if (entries.some((entry) => entry.target === state.element && entry.isIntersecting)) {
            state.observer?.disconnect();
            state.observer = null;
            void activateState(state).catch(() => {});
          }
        });
        state.observer.observe(state.element);
        break;
      case "media":
        if (typeof matchMediaImpl !== "function") fail("media activation requires matchMedia");
        state.media = matchMediaImpl(state.record.media);
        if (state.media.matches) {
          queueMicrotaskImpl(() => void activateState(state).catch(() => {}));
        } else {
          const onChange = (event) => {
            if (!event.matches) return;
            state.media.removeEventListener?.("change", onChange);
            void activateState(state).catch(() => {});
          };
          state.media.addEventListener?.("change", onChange, { once: true });
          state.mediaListener = onChange;
        }
        break;
      case "interaction":
        break;
      default:
        fail(`unhandled activation ${state.record.activation}`);
    }
  }

  const loader = {
    activate(key) {
      const state = states.get(key);
      if (!state) fail(`unknown island key ${key}`);
      return activateState(state);
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      for (const [name, listener] of listeners) root.removeEventListener(name, listener, false);
      listeners.clear();
      for (const [name, listener] of prefetchListeners) root.removeEventListener(name, listener, false);
      prefetchListeners.length = 0;
      for (const state of states.values()) {
        state.observer?.disconnect();
        if (state.idle != null) cancelIdle(state.idle);
        if (state.mediaListener) state.media?.removeEventListener?.("change", state.mediaListener);
        state.prefetchObserver?.disconnect();
        if (state.prefetchIdle != null) cancelIdle(state.prefetchIdle);
        if (state.prefetchMediaListener) state.prefetchMedia?.removeEventListener?.("change", state.prefetchMediaListener);
        state.application?.dispose();
        state.queued.length = 0;
        state.status = "disposed";
      }
    },
    status(key) {
      const state = states.get(key);
      if (!state) fail(`unknown island key ${key}`);
      return state.status;
    },
  };
  if (options.development === true) {
    loader.inspectDevelopment = () => Object.freeze({
      schema: "witchy.glamour.island-devtools.v1",
      buildIdentity: manifest.buildIdentity,
      disposed,
      islands: Object.freeze(manifest.islands.map((record) => {
        const state = states.get(record.key);
        const model = state.status === "active"
          ? checkedRedactedDevelopmentModel(state.application, record.key)
          : null;
        return Object.freeze({
          id: record.id,
          artifact: record.artifact,
          key: record.key,
          mode: record.mode,
          parent: state.parent,
          policy: record.activation,
          status: state.status,
          activation: state.mode,
          prefetch: state.prefetchStatus,
          eventPlans: record.events.length,
          queuedEvents: state.queued.length,
          ...(model === null ? {} : { model }),
        });
      })),
    });
  }
  return Object.freeze(loader);
}

/** Install a complete compiler-published island graph.
 *
 * Prefetch retrieves immutable bytes only. Compilation, custom-section grant
 * authentication, UiRoot binding, and mounting begin at activation.
 */
export function installPublishedIslands(options = {}) {
  const manifest = checkedManifest(options.manifest);
  const publication = checkedArtifactPublication(options.artifacts, manifest);
  if (typeof options.mountArtifact !== "function") fail("published islands require a mountArtifact function");
  const fetchImpl = options.fetch || globalThis.fetch;
  if (typeof fetchImpl !== "function") fail("published islands require fetch");
  const compile = options.compile || WebAssembly.compile;
  const customSections = options.customSections || WebAssembly.Module.customSections;
  if (typeof compile !== "function" || typeof customSections !== "function") {
    fail("published islands require WebAssembly compilation and custom sections");
  }
  const byteCache = new Map();
  const moduleCache = new Map();

  const artifactFor = (record) => {
    const artifact = publication.byIdentity.get(record.artifact);
    if (!artifact) fail(`island ${record.id} has no authenticated artifact`);
    return artifact;
  };
  const fetchBytes = (artifact) => {
    if (byteCache.has(artifact.artifact)) return byteCache.get(artifact.artifact);
    const pending = Promise.resolve(fetchImpl(artifact.url, { credentials: "same-origin" }))
      .then(async (response) => {
        if (!response?.ok || typeof response.arrayBuffer !== "function") fail(`artifact ${artifact.artifact} could not be fetched`);
        const declared = Number(response.headers?.get?.("content-length"));
        if (Number.isFinite(declared) && declared > MAX_ARTIFACT_BYTES) fail(`artifact ${artifact.artifact} exceeds its byte limit`);
        const bytes = await response.arrayBuffer();
        if (!(bytes instanceof ArrayBuffer) || bytes.byteLength === 0 || bytes.byteLength > MAX_ARTIFACT_BYTES) {
          fail(`artifact ${artifact.artifact} is empty or oversized`);
        }
        return bytes;
      })
      .catch((error) => {
        byteCache.delete(artifact.artifact);
        throw error;
      });
    byteCache.set(artifact.artifact, pending);
    return pending;
  };
  const compileArtifact = (artifact) => {
    if (moduleCache.has(artifact.artifact)) return moduleCache.get(artifact.artifact);
    const pending = fetchBytes(artifact).then(async (bytes) => {
      const module = await compile(bytes);
      const sections = customSections(module, "witchy.web.mount-grant");
      if (!Array.isArray(sections) || sections.length !== 1) {
        fail(`artifact ${artifact.artifact} mount grant is missing or duplicated`);
      }
      let embedded;
      try {
        embedded = JSON.parse(decoder.decode(sections[0]));
      } catch {
        fail(`artifact ${artifact.artifact} mount grant is malformed`);
      }
      if (
        embedded?.schema !== "witchy.web.mount-grant-section.v1" ||
        embedded.artifact !== artifact.artifact ||
        JSON.stringify(embedded.grant) !== JSON.stringify(manifest.mountGrant) ||
        JSON.stringify(embedded.artifactGrant) !== JSON.stringify(artifact.grantProjection)
      ) {
        fail(`artifact ${artifact.artifact} mount grant differs from publication`);
      }
      return module;
    }).catch((error) => {
      moduleCache.delete(artifact.artifact);
      throw error;
    });
    moduleCache.set(artifact.artifact, pending);
    return pending;
  };

  return installIslands({
    ...options,
    manifest: options.manifest,
    applicationOwnsEvents: true,
    prefetch: (record) => fetchBytes(artifactFor(record)).then(() => undefined),
    load: async (record) => {
      const artifact = artifactFor(record);
      const module = await compileArtifact(artifact);
      const mount = (element, input, mode) => options.mountArtifact(Object.freeze({
        module,
        element,
        artifact,
        mountGrant: manifest.mountGrant,
        mode,
        state: input.state ?? null,
        trigger: input.trigger ?? null,
      }));
      return Object.freeze({
        identity: artifact.artifact,
        buildIdentity: artifact.buildIdentity,
        resume: (element, input) => mount(element, input, "resume"),
        fresh: (element, input) => mount(element, input, "fresh"),
      });
    },
  });
}

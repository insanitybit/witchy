#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { IslandDomMismatch, installIslands, installPublishedIslands } from "./glamour-islands.mjs";
import { FakeElement } from "./glamour-test-dom.mjs";

const BUILD = "a".repeat(64);
const ARTIFACT = `glamour-island1-${"b".repeat(64)}`;
const MEDIA_CORPUS = JSON.parse(
  readFileSync(new URL("./glamour-media-query-corpus.json", import.meta.url), "utf8"),
);

function instance(digit) {
  return `glamour-instance1-${digit.repeat(64)}`;
}

function tree(records) {
  const root = new FakeElement("root");
  root.getAttribute = (name) => root.attributes.get(name) ?? null;
  root.querySelectorAll = (selector) => {
    assert.equal(selector, "[data-glamour-island]");
    return records.map(({ id, node = 20 }) => {
      const island = new FakeElement("section");
      island.getAttribute = (name) => island.attributes.get(name) ?? null;
      island.setAttribute("data-glamour-island", id);
      island.setAttribute("data-glamour-build", BUILD);
      const button = new FakeElement("button");
      button.getAttribute = (name) => button.attributes.get(name) ?? null;
      button.setAttribute("data-glamour-node", String(node));
      island.appendChild(button);
      root.appendChild(island);
      return island;
    });
  };
  const islands = root.querySelectorAll("[data-glamour-island]");
  root.querySelectorAll = () => islands;
  return { root, islands, buttons: islands.map((island) => island.childNodes[0]) };
}

function record(key, id, activation = "interaction") {
  return {
    id,
    artifact: ARTIFACT,
    key,
    mode: "resume",
    activation,
    media: activation === "media" ? "(min-width: 40rem)" : null,
    prefetch: "none",
    prefetchMedia: null,
    name: null,
    state: "0",
    events: [{ name: "click", node: 20, plan: 31, preventDefault: true }],
  };
}

function freshRecord(key, id, activation = "interaction") {
  return {
    ...record(key, id, activation),
    mode: "fresh",
    state: null,
    events: [],
  };
}

function manifest(islands) {
  return { schema: "witchy.glamour.islands.v1", buildIdentity: BUILD, islands };
}

function click(root, island, button) {
  let prevented = 0;
  root.dispatchEvent({
    type: "click",
    target: button,
    isTrusted: true,
    composedPath: () => [button, island, root],
    preventDefault: () => { prevented += 1; },
  });
  return () => prevented;
}

{
  const item = record("unsafe-debug", instance("0"));
  const { root } = tree([item]);
  const loader = installIslands({
    root,
    development: true,
    manifest: manifest([item]),
    load: async () => ({
      identity: ARTIFACT,
      buildIdentity: BUILD,
      resume: async () => ({
        inspectDevelopment: () => ({
          schema: "witchy.glamour.devtools.v1",
          model: {
            schema: "d".repeat(64),
            snapshotFormat: 0,
            fields: [{ index: 0, name: "secret", kind: "Aggregate", value: "private" }],
          },
        }),
        dispose() {},
      }),
    }),
  });
  await loader.activate("unsafe-debug");
  assert.throws(
    () => loader.inspectDevelopment(),
    /development model field exposed a value/,
  );
  loader.dispose();
}

{
  const first = record("first", instance("1"));
  const second = record("second", instance("2"));
  const { root, islands, buttons } = tree([first, second]);
  islands[1].parentNode = islands[0];
  const loaded = [];
  const triggers = [];
  const disposed = [];
  const loader = installIslands({
    root,
    development: true,
    manifest: manifest([first, second]),
    load: async (item) => {
      loaded.push(item.key);
      return {
        identity: ARTIFACT,
        buildIdentity: BUILD,
        resume: async (_element, input) => {
          triggers.push([item.key, input.trigger]);
          return {
            dispatch: async (trigger) => triggers.push([item.key, trigger]),
            inspectDevelopment: () => ({
              schema: "witchy.glamour.devtools.v1",
              model: {
                schema: "c".repeat(64),
                snapshotFormat: 0,
                fields: [
                  { index: 0, name: "count", kind: "Int", value: "<redacted>" },
                  { index: 1, name: "items", kind: "Aggregate", value: "<redacted>" },
                ],
              },
            }),
            dispose: () => disposed.push(item.key),
          };
        },
      };
    },
  });

  assert.deepEqual(loaded, [], "interaction islands load no application code initially");
  const initialInspection = loader.inspectDevelopment();
  assert.equal(initialInspection.schema, "witchy.glamour.island-devtools.v1");
  assert.equal(initialInspection.buildIdentity, BUILD);
  assert.equal(initialInspection.disposed, false);
  assert.deepEqual(
    initialInspection.islands.map(({ key, parent, policy, status, activation, eventPlans, queuedEvents }) => ({
      key,
      parent,
      policy,
      status,
      activation,
      eventPlans,
      queuedEvents,
    })),
    [
      {
        key: "first",
        parent: null,
        policy: "interaction",
        status: "inert",
        activation: null,
        eventPlans: 1,
        queuedEvents: 0,
      },
      {
        key: "second",
        parent: first.id,
        policy: "interaction",
        status: "inert",
        activation: null,
        eventPlans: 1,
        queuedEvents: 0,
      },
    ],
  );
  assert.ok(Object.isFrozen(initialInspection));
  assert.ok(Object.isFrozen(initialInspection.islands));
  assert.ok(initialInspection.islands.every(Object.isFrozen));
  assert.ok(!JSON.stringify(initialInspection).includes('"state"'));
  assert.ok(!JSON.stringify(initialInspection).includes('"application"'));
  assert.ok(!JSON.stringify(initialInspection).includes('"element"'));
  const prevented = click(root, islands[0], buttons[0]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(prevented(), 1);
  assert.deepEqual(loaded, ["first"], "activating one island does not load its sibling");
  assert.equal(triggers.length, 1, "the activating event is delivered exactly once");
  assert.deepEqual(triggers[0], ["first", {
    plan: 31,
    node: 20,
    name: "click",
    value: "",
    checked: false,
    key: "",
    composing: false,
    userActivation: true,
  }]);
  assert.equal(loader.status("first"), "active");
  assert.equal(loader.status("second"), "inert");
  assert.deepEqual(
    loader.inspectDevelopment().islands.map(({ status, activation }) => ({ status, activation })),
    [
      { status: "active", activation: "resume" },
      { status: "inert", activation: null },
    ],
  );
  const model = loader.inspectDevelopment().islands[0].model;
  assert.deepEqual(model, {
    schema: "c".repeat(64),
    snapshotFormat: 0,
    fields: [
      { index: 0, name: "count", kind: "Int", value: "<redacted>" },
      { index: 1, name: "items", kind: "Aggregate", value: "<redacted>" },
    ],
  });
  assert.ok(Object.isFrozen(model));
  assert.ok(Object.isFrozen(model.fields));
  assert.ok(model.fields.every(Object.isFrozen));
  assert.ok(!JSON.stringify(model).includes("private"));
  loader.dispose();
  const disposedInspection = loader.inspectDevelopment();
  assert.equal(disposedInspection.disposed, true);
  assert.deepEqual(disposedInspection.islands.map(({ status }) => status), ["disposed", "disposed"]);
  assert.deepEqual(disposed, ["first"]);
}

{
  const item = record("fallback", instance("3"), "interaction");
  const { root } = tree([item]);
  let fresh = 0;
  const loader = installIslands({
    root,
    development: true,
    manifest: manifest([item]),
    load: async () => ({
      identity: ARTIFACT,
      buildIdentity: BUILD,
      resume: async () => { throw new IslandDomMismatch("stale static tree"); },
    }),
    freshMount: async (_element, checked, _artifact, trigger, error) => {
      assert.equal(checked.key, "fallback");
      assert.equal(trigger, null);
      assert.match(error.message, /stale static tree/);
      fresh += 1;
      return { dispose() {} };
    },
  });
  await loader.activate("fallback");
  assert.equal(fresh, 1, "only an explicit resume mismatch selects controlled fresh mount");
  assert.equal(loader.inspectDevelopment().islands[0].activation, "fresh-from-public-state");
  loader.dispose();
}

{
  const item = freshRecord("editor", instance("e"));
  const { root, islands, buttons } = tree([item]);
  const starts = [];
  const loader = installIslands({
    root,
    development: true,
    manifest: manifest([item]),
    load: async () => ({
      identity: ARTIFACT,
      buildIdentity: BUILD,
      resume: async () => assert.fail("a fresh client region must not resume public state"),
      fresh: async (_element, input) => {
        starts.push(input);
        return { dispose() {} };
      },
    }),
  });
  const prevented = click(root, islands[0], buttons[0]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(prevented(), 0, "a fresh activation gesture preserves native browser behavior");
  assert.deepEqual(starts, [{ trigger: null }], "a fresh activation gesture starts once and is never replayed as a message");
  assert.equal(loader.inspectDevelopment().islands[0].mode, "fresh");
  assert.equal(loader.inspectDevelopment().islands[0].activation, "fresh");
  loader.dispose();
}

{
  const item = record("queued", instance("6"));
  const { root, islands, buttons } = tree([item]);
  let release;
  const ready = new Promise((resolve) => { release = resolve; });
  const delivered = [];
  const loader = installIslands({
    root,
    manifest: manifest([item]),
    load: async () => {
      await ready;
      return {
        identity: ARTIFACT,
        buildIdentity: BUILD,
        resume: async (_element, input) => {
          delivered.push(["resume", input.trigger]);
          return {
            dispatch: async (trigger) => delivered.push(["dispatch", trigger]),
            dispose() {},
          };
        },
      };
    },
  });
  assert.equal(loader.inspectDevelopment, undefined);
  click(root, islands[0], buttons[0]);
  click(root, islands[0], buttons[0]);
  assert.equal(loader.status("queued"), "loading");
  release();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(
    delivered.map(([path]) => path),
    ["resume", "dispatch"],
    "the first trigger resumes once and later loading-time triggers dispatch once in order",
  );
  loader.dispose();
}

{
  const item = record("failed", instance("7"), "interaction");
  const { root } = tree([item]);
  const loader = installIslands({
    root,
    development: true,
    manifest: manifest([item]),
    load: async () => ({
      identity: `glamour-island1-${"c".repeat(64)}`,
      buildIdentity: BUILD,
      resume: async () => assert.fail("an identity mismatch must not resume"),
    }),
    freshMount: async () => assert.fail("an identity mismatch must not fresh-mount"),
  });
  await assert.rejects(loader.activate("failed"), /artifact identity does not match/);
  assert.deepEqual(
    loader.inspectDevelopment().islands.map(({ status, activation, queuedEvents }) => ({
      status,
      activation,
      queuedEvents,
    })),
    [{ status: "failed", activation: null, queuedEvents: 0 }],
  );
  loader.dispose();
}

{
  const item = record("recover-link", instance("9"), "interaction");
  item.events[0].fallback = { kind: "navigate", href: "/safe" };
  const { root, islands, buttons } = tree([item]);
  buttons[0].tag = "a";
  buttons[0].setAttribute("href", "/safe");
  const navigations = [];
  const loader = installIslands({
    root,
    manifest: manifest([item]),
    navigate: (href, node) => navigations.push([href, node]),
    load: async () => { throw new Error("offline"); },
  });
  const prevented = click(root, islands[0], buttons[0]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(prevented(), 1);
  assert.deepEqual(navigations, [["/safe", buttons[0]]], "a prevented dormant navigation recovers exactly once");
  assert.equal(loader.status("recover-link"), "failed");
  loader.dispose();
}

{
  const item = record("recover-form", instance("d"), "interaction");
  item.events = [{
    name: "submit",
    node: 20,
    plan: 32,
    preventDefault: true,
    fallback: { kind: "submit", action: "/save", method: "post" },
  }];
  const { root, islands, buttons } = tree([item]);
  const form = buttons[0];
  form.tag = "form";
  form.setAttribute("action", "/save");
  form.setAttribute("method", "post");
  const submissions = [];
  const loader = installIslands({
    root,
    manifest: manifest([item]),
    submit: (node, submitter, contract) => submissions.push([node, submitter, contract]),
    load: async () => { throw new Error("bad wasm"); },
  });
  let prevented = 0;
  root.dispatchEvent({
    type: "submit",
    target: form,
    submitter: null,
    isTrusted: true,
    composedPath: () => [form, islands[0], root],
    preventDefault: () => { prevented += 1; },
  });
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(prevented, 1);
  assert.equal(submissions.length, 1, "a prevented dormant submission recovers exactly once");
  assert.equal(submissions[0][0], form);
  assert.deepEqual(submissions[0][2], { action: "/save", method: "post" });
  loader.dispose();
}

assert.throws(
  () => installIslands({
    root: tree([]).root,
    manifest: manifest([{ ...record("bad-fallback", instance("f")), events: [{
      name: "click",
      node: 20,
      plan: 31,
      preventDefault: false,
      fallback: { kind: "navigate", href: "/safe" },
    }] }]),
    load: async () => assert.fail("invalid fallback must fail before loading"),
  }),
  /fallback without prevention/,
);

{
  const visible = record("visible", instance("4"), "visible");
  const { root, islands, buttons } = tree([visible]);
  let callback;
  let loads = 0;
  let trigger = null;
  class Observer {
    constructor(next) { callback = next; }
    observe() {}
    disconnect() {}
  }
  const loader = installIslands({
    root,
    manifest: manifest([visible]),
    IntersectionObserver: Observer,
    load: async () => {
      loads += 1;
      return {
        identity: ARTIFACT,
        buildIdentity: BUILD,
        resume: async (_element, input) => {
          trigger = input.trigger;
          return { dispose() {} };
        },
      };
    },
  });
  assert.equal(loads, 0);
  click(root, islands[0], buttons[0]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(loads, 1, "an authenticated interaction closes the visible-observer race");
  assert.equal(trigger.plan, 31);
  callback([{ target: islands[0], isIntersecting: true }]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(loads, 1, "the later observer callback cannot activate twice");
  loader.dispose();
}

{
  const visible = { ...record("prefetch-visible", instance("8"), "visible"), prefetch: "visible" };
  const { root, islands } = tree([visible]);
  const observers = [];
  const operations = [];
  class Observer {
    constructor(callback, options) { this.callback = callback; this.options = options; observers.push(this); }
    observe() {}
    disconnect() {}
  }
  const loader = installIslands({
    root,
    development: true,
    manifest: manifest([visible]),
    IntersectionObserver: Observer,
    prefetch: async (item) => operations.push(["prefetch", item.key]),
    load: async (item) => {
      operations.push(["load", item.key]);
      return {
        identity: ARTIFACT,
        buildIdentity: BUILD,
        resume: async () => ({ dispose() {} }),
      };
    },
  });
  const prefetchObserver = observers.find((observer) => observer.options?.rootMargin === "100% 0px");
  const activationObserver = observers.find((observer) => observer.options == null);
  assert.ok(prefetchObserver, "visible prefetch uses a one-viewport lookahead observer");
  assert.ok(activationObserver, "visible activation uses the actual viewport observer");
  prefetchObserver.callback([{ target: islands[0], isIntersecting: true }]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(operations, [["prefetch", "prefetch-visible"]]);
  assert.equal(loader.status("prefetch-visible"), "inert", "prefetch does not instantiate or activate");
  assert.equal(loader.inspectDevelopment().islands[0].prefetch, "prefetched");
  activationObserver.callback([{ target: islands[0], isIntersecting: true }]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(operations, [["prefetch", "prefetch-visible"], ["load", "prefetch-visible"]]);
  assert.equal(loader.status("prefetch-visible"), "active");
  loader.dispose();
}

{
  const item = record("bad", instance("5"));
  const { root } = tree([item]);
  assert.throws(
    () => installIslands({
      root,
      manifest: manifest([{ ...item, state: "not-json" }]),
      load: async () => assert.fail("invalid state must fail before code load"),
    }),
    /public state is not JSON/,
  );
  assert.throws(
    () => installIslands({
      root,
      manifest: manifest([{ ...freshRecord("fresh-state", instance("a")), state: "0" }]),
      load: async () => assert.fail("fresh public state must fail before code load"),
    }),
    /must not contain public state/,
  );
  assert.throws(
    () => installIslands({
      root,
      manifest: manifest([{ ...item, mode: "guess" }]),
      load: async () => assert.fail("unknown mode must fail before code load"),
    }),
    /mode is invalid/,
  );
}

for (const [index, example] of MEDIA_CORPUS.entries()) {
  const item = { ...record(`media-${index}`, instance((index % 10).toString()), "media"), media: example.query };
  const { root } = tree([item]);
  if (example.valid) {
    installIslands({
      root,
      manifest: manifest([item]),
      matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
      load: async () => assert.fail("media corpus validation must not load"),
    }).dispose();
  } else {
    assert.throws(
      () => installIslands({ root, manifest: manifest([item]), load: async () => assert.fail("invalid media must not load") }),
      /media query.*invalid|media query is invalid or oversized/,
      `loader media disagreement for ${JSON.stringify(example.query)}`,
    );
  }
}

{
  const grant = {
    schema: "witchy.web.ui-root-grant.v1",
    parameter: "ui",
    capability: "UiRoot",
    policy: "published-test",
    digest: "c".repeat(64),
  };
  const item = { ...record("published", instance("9"), "load"), grantDigest: "d".repeat(64) };
  const publishedManifest = { ...manifest([item]), mountGrant: grant };
  const browserPolicy = {
    schema: "witchy.glamour.browser-policy.v1",
    fetch: [], navigation: [], timers: [{ minimum: 10 }], ports: ["credential.get-exchange.v1"],
    secretFields: [{ form: `glamour-form1-${"e".repeat(64)}`, field: "password" }],
    frames: [], workers: [], storage: [{
      provider: "local", namespace: "preferences", keyPrefix: "book.", maxValueBytes: 4096,
    }],
  };
  const effectPolicy = { kind: "timer", minimum: 10 };
  const effectGrant = { semantic: "timer", policy: effectPolicy };
  const subscriptionGrant = { semantic: "interval", policy: effectPolicy };
  const storagePolicy = {
    kind: "storage", provider: "local", namespace: "preferences", keyPrefix: "book.", maxValueBytes: 4096,
  };
  const storageGrant = { semantic: "storage-get", policy: storagePolicy };
  const hostPortPolicy = {
    kind: "host-port", adapter: "credential.get-exchange.v1",
    endpoint: "/auth/passkey/exchange", maxRequestBytes: 61_440, maxResultBytes: 512,
  };
  const hostPortGrant = { semantic: "host-port", policy: hostPortPolicy };
  const action = {
    id: `glamour-form1-${"e".repeat(64)}`,
    method: "POST",
    action: "/login",
    fields: [{ name: "password", label: "Password", kind: "secret", required: true }],
    inputSchema: 1,
    resultSchema: 2,
  };
  const staticControls = {
    schema: "witchy.glamour.static-controls.v1",
    actions: [action],
  };
  const grantProjection = {
    schema: "witchy.glamour.artifact-grant.v1",
    projectGrantDigest: grant.digest,
    effects: { 37: effectGrant, 46: storageGrant, 50: hostPortGrant },
    subscriptions: { 41: subscriptionGrant },
    staticControls,
    browserPolicy,
  };
  const artifact = {
    artifact: ARTIFACT,
    wireId: 11,
    registryId: 12,
    buildIdentity: BUILD,
    grantDigest: grant.digest,
    grantProjection,
    browserPolicy,
    actions: [action],
    appId: 11,
    buildId: `0x${BUILD.slice(0, 16)}`,
    features: { mode: "production", startupBarrier: true },
    limits: {},
    url: "/assets/island-deadbeef.wasm",
    moduleGroup: "island-deadbeef.wasm",
    programTypes: {},
    templates: [], nodes: [], regions: [], attributeBindings: [],
    properties: {}, attributes: {}, aria: {}, customProperties: {},
    ownerInstances: {},
    eventClasses: [], eventPlans: [],
    effectDescriptors: {
      37: { handler: "timer", resultSchema: 38, completion: 39, ownerScope: 44, semantic: "timer", policy: effectPolicy },
      46: { handler: "storage", resultSchema: 47, completion: 48, ownerScope: 49, semantic: "storage-get", policy: storagePolicy },
      50: { handler: "port", resultSchema: 51, completion: 52, ownerScope: 53, semantic: "host-port", policy: hostPortPolicy },
    },
    subscriptionDescriptors: { 41: { handler: "interval", resultSchema: 42, completion: 43, ownerScope: 45, semantic: "interval", policy: effectPolicy } },
    frames: [],
    fresh: null,
    resume: {},
  };
  const publication = {
    schema: "witchy.glamour.island-artifacts.v1",
    buildIdentity: BUILD,
    grantDigest: grant.digest,
    artifacts: [artifact],
    workers: [],
    frames: [],
  };
  const embedded = new TextEncoder().encode(JSON.stringify({
    schema: "witchy.web.mount-grant-section.v1",
    grant,
    artifact: ARTIFACT,
    artifactGrant: grantProjection,
  })).buffer;
  const { root, islands, buttons } = tree([item]);
  const mounts = [];
  let schedulerDispatches = 0;
  let fetches = 0;
  let compilations = 0;
  const loader = installPublishedIslands({
    root,
    manifest: publishedManifest,
    artifacts: publication,
    queueMicrotask: (callback) => callback(),
    fetch: async () => {
      fetches += 1;
      return { ok: true, headers: { get: () => "4" }, arrayBuffer: async () => new ArrayBuffer(4) };
    },
    compile: async () => {
      compilations += 1;
      return { compiled: true };
    },
    customSections: () => [embedded],
    mountArtifact: async (input) => {
      mounts.push(input);
      return {
        dispatch: () => { schedulerDispatches += 1; },
        dispose() {},
      };
    },
  });
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(loader.status("published"), "active");
  assert.equal(fetches, 1);
  assert.equal(compilations, 1);
  assert.equal(mounts[0].artifact.artifact, ARTIFACT);
  assert.equal(mounts[0].mountGrant.digest, grant.digest);
  assert.equal(mounts[0].mode, "resume");
  click(root, islands[0], buttons[0]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(schedulerDispatches, 0, "a published active application owns its DOM events");
  loader.dispose();

  const malformed = structuredClone(publication);
  malformed.artifacts[0].grantProjection.effects = {};
  assert.throws(
    () => installPublishedIslands({ root: tree([item]).root, manifest: publishedManifest, artifacts: malformed, mountArtifact() {} }),
    /effect grants differs/,
  );

  const forgedPolicy = structuredClone(publication);
  forgedPolicy.artifacts[0].effectDescriptors[37].policy = { kind: "timer", minimum: 0 };
  assert.throws(
    () => installPublishedIslands({ root: tree([item]).root, manifest: publishedManifest, artifacts: forgedPolicy, mountArtifact() {} }),
    /differs from its descriptor/,
  );

  const forgedControls = structuredClone(publication);
  forgedControls.artifacts[0].actions.push({
    id: `glamour-form1-${"f".repeat(64)}`,
    method: "POST",
    action: "/login",
    fields: [],
    inputSchema: 1,
    resultSchema: 2,
  });
  assert.throws(
    () => installPublishedIslands({ root: tree([item]).root, manifest: publishedManifest, artifacts: forgedControls, mountArtifact() {} }),
    /static controls differs from the published actions/,
  );
}

console.log("GLAMOUR-ISLANDS OK");

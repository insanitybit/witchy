import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  ProgressiveFormSubmission,
  decodeProgressiveForm,
  installProgressiveForms,
} from "./glamour-forms.mjs";
import { FakeElement } from "./glamour-test-dom.mjs";

const fixtures = JSON.parse(
  readFileSync(
    new URL("../../projects/glamour/form-decoder-fixtures.json", import.meta.url),
    "utf8",
  ),
);
const action = fixtures.action;
const transportAction = Object.freeze({ ...action, inputSchema: 47, resultSchema: 53 });
const accepted = fixtures.cases.find(({ name }) => name === "accepted");
const rejectedCase = fixtures.cases.find(({ name }) => name === "rejected");

test("browser decoder partitions public and secret values like FormSchema", () => {
  const decoded = decodeProgressiveForm(action, accepted.entries);

  assert.equal(decoded.ok, true);
  assert.ok(decoded.submission instanceof ProgressiveFormSubmission);
  assert.deepEqual(
    decoded.submission.values.map(({ name, value }) => [name, value]),
    accepted.public,
  );
  assert.throws(() => JSON.stringify(decoded.submission), /not serializable/);
  for (const [name, value] of accepted.secrets) {
    assert.equal(decoded.submission.takeSecret(name), value);
    assert.equal(decoded.submission.takeSecret(name), undefined);
  }
});

test("browser decoder has deterministic duplicate, unknown, and required failures", () => {
  const decoded = decodeProgressiveForm(action, rejectedCase.entries);

  assert.equal(decoded.ok, false);
  assert.deepEqual(
    decoded.problems.map(({ message }) => message),
    rejectedCase.problems,
  );
});

test("browser decoder rejects malformed schemas, files, and invalid field kinds", () => {
  assert.equal(decodeProgressiveForm({}, []).problems[0].code, "invalid-schema");
  assert.equal(
    decodeProgressiveForm({ ...action, method: "GET" }, []).problems[0].message,
    "secret progressive form fields require POST",
  );

  const fileLike = decodeProgressiveForm(action, [
    ["name", { name: "avatar.png" }],
    ["email", "ada@example.test"],
    ["password", "secret"],
  ]);
  assert.equal(fileLike.problems[0].code, "invalid-field");

  const invalid = decodeProgressiveForm(action, [
    ["name", "Ada"],
    ["email", "not-an-email"],
    ["password", "secret"],
  ]);
  assert.equal(invalid.problems[0].message, "invalid value for form field `email`");
});

class FixtureFormData {
  constructor(form) {
    this.form = form;
  }

  entries() {
    return this.form.entries[Symbol.iterator]();
  }
}

function fixtureForm(entries = accepted.entries) {
  const form = new FakeElement("form");
  form.entries = entries;
  form.setAttribute("data-glamour-form", action.id);
  form.setAttribute("method", action.method);
  form.setAttribute("action", action.action);
  return form;
}

function submitEvent(form) {
  return {
    type: "submit",
    target: form,
    defaultPrevented: false,
    composedPath: () => [form, form.parentNode],
    preventDefault() {
      this.defaultPrevented = true;
    },
  };
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

test("progressive enhancement owns secrets and publishes an explicit lifecycle", async () => {
  const root = new FakeElement("main");
  const form = fixtureForm();
  root.appendChild(form);
  const states = [];
  const lifecycle = [];
  const requests = [];
  const enhancement = installProgressiveForms({
    root,
    actions: [transportAction],
    FormData: FixtureFormData,
    baseUrl: "https://witchy.example/book",
    onState: (state) => states.push(state),
    onLifecycle: (state, schema) => lifecycle.push({ state, schema }),
    fetch: async (url, init) => {
      requests.push({ url, init });
      return { ok: true, status: 204 };
    },
  });
  const event = submitEvent(form);

  await [...root.listeners.get("submit")][0](event);

  assert.equal(event.defaultPrevented, true);
  assert.deepEqual(states.map(({ phase }) => phase), [
    "Validating",
    "Submitting",
    "Succeeded",
  ]);
  assert.deepEqual(states[1].values, [
    { name: "name", value: "Ada" },
    { name: "email", value: "ada@example.test" },
  ]);
  assert.doesNotMatch(JSON.stringify(states), /s3cret/);
  assert.equal(lifecycle[1].schema.inputSchema, 47);
  assert.equal(lifecycle[2].schema.resultSchema, 53);
  assert.doesNotMatch(JSON.stringify(lifecycle), /s3cret/);
  assert.equal(requests[0].url, "https://witchy.example/signup");
  assert.equal(requests[0].init.method, "POST");
  assert.equal(requests[0].init.redirect, "manual");
  assert.equal(
    requests[0].init.body.toString(),
    "name=Ada&email=ada%40example.test&password=s3cret",
  );
  assert.equal(enhancement.activeCount, 0);
  assert.equal(form.attributes.get("data-glamour-form-state"), "succeeded");
  assert.equal(form.attributes.has("aria-busy"), false);
  const nextRoot = new FakeElement("main");
  enhancement.rebind(nextRoot);
  assert.equal(root.listeners.get("submit").size, 0);
  assert.equal(nextRoot.listeners.get("submit").size, 1);
  enhancement.dispose();
  assert.equal(enhancement.disposed, true);
  assert.equal(nextRoot.listeners.get("submit").size, 0);
});

test("progressive enhancement reports validation and HTTP failures without sending", async () => {
  const root = new FakeElement("main");
  const invalidForm = fixtureForm([
    ["name", ""],
    ["email", "bad"],
    ["password", "secret"],
  ]);
  root.appendChild(invalidForm);
  const states = [];
  let requests = 0;
  installProgressiveForms({
    root,
    actions: [transportAction],
    FormData: FixtureFormData,
    baseUrl: "https://witchy.example/",
    onState: (state) => states.push(state),
    fetch: async () => {
      requests += 1;
      return { ok: false, status: 422 };
    },
  });

  await [...root.listeners.get("submit")][0](submitEvent(invalidForm));
  assert.equal(requests, 0);
  assert.equal(states.at(-1).phase, "Failed");
  assert.equal(states.at(-1).reason, "validation");

  invalidForm.entries = accepted.entries;
  await [...root.listeners.get("submit")][0](submitEvent(invalidForm));
  assert.equal(requests, 1);
  assert.equal(states.at(-1).phase, "Failed");
  assert.equal(states.at(-1).reason, "server");
  assert.equal(states.at(-1).result.status, 422);
});

test("new submissions cancel stale work and cross-origin forms retain native behavior", async () => {
  const root = new FakeElement("main");
  const form = fixtureForm();
  root.appendChild(form);
  const states = [];
  const first = deferred();
  let requests = 0;
  const enhancement = installProgressiveForms({
    root,
    actions: [transportAction],
    FormData: FixtureFormData,
    baseUrl: "https://witchy.example/",
    onState: (state) => states.push(state),
    fetch: (_url, init) => {
      requests += 1;
      if (requests === 1) {
        init.signal.addEventListener("abort", () => first.reject(new Error("aborted")));
        return first.promise;
      }
      return Promise.resolve({ ok: true, status: 200 });
    },
  });
  const listener = [...root.listeners.get("submit")][0];
  const firstRun = listener(submitEvent(form));
  const secondRun = listener(submitEvent(form));
  await Promise.all([firstRun, secondRun]);

  assert.equal(requests, 2);
  assert.deepEqual(states.map(({ phase }) => phase), [
    "Validating",
    "Submitting",
    "Cancelled",
    "Validating",
    "Submitting",
    "Succeeded",
  ]);
  assert.equal(states[2].reason, "superseded");
  assert.equal(states.at(-1).generation, 2);

  const external = fixtureForm();
  external.setAttribute("action", "https://other.example/signup");
  const externalAction = {
    ...transportAction,
    action: "https://other.example/signup",
  };
  enhancement.dispose();
  const externalRoot = new FakeElement("main");
  externalRoot.appendChild(external);
  installProgressiveForms({
    root: externalRoot,
    actions: [externalAction],
    FormData: FixtureFormData,
    baseUrl: "https://witchy.example/",
    fetch: () => {
      throw new Error("cross-origin form must not be intercepted");
    },
  });
  const externalEvent = submitEvent(external);
  await [...externalRoot.listeners.get("submit")][0](externalEvent);
  assert.equal(externalEvent.defaultPrevented, false);
});

console.log("GLAMOUR-FORMS OK");

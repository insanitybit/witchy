// Progressive forms at the browser trust boundary. The manifest owns the
// schema. Application code receives only inert public values and lifecycle
// records; secret values remain inside the host-owned request path.

const FIELD_KINDS = new Set(["text", "email", "number", "checkbox", "secret"]);
const MAX_FIELDS = 256;
const MAX_ENTRIES = 512;
const MAX_FIELD_CHARACTERS = 65_536;

export class ProgressiveFormSubmission {
  #secrets;

  constructor(values, secrets) {
    this.values = Object.freeze(values.map(({ name, value }) => Object.freeze({ name, value })));
    this.#secrets = new Map(secrets);
    Object.freeze(this);
  }

  takeSecret(name) {
    const value = this.#secrets.get(name);
    this.#secrets.delete(name);
    return value;
  }

  toJSON() {
    throw new TypeError("Glamour server-secret form values are not serializable");
  }
}

export function decodeProgressiveForm(action, entries) {
  const schemaProblem = validateAction(action);
  if (schemaProblem) {
    return rejected([schemaProblem]);
  }

  const declared = new Map(action.fields.map((field) => [field.name, field]));
  const seen = new Set();
  const values = [];
  const secrets = [];
  const problems = [];
  if (entries === null || entries === undefined || typeof entries[Symbol.iterator] !== "function") {
    return rejected([problem("invalid-entry", "", "invalid submitted form entry")]);
  }
  let entryCount = 0;
  for (const entry of entries) {
    entryCount += 1;
    if (entryCount > MAX_ENTRIES) {
      problems.push(problem("too-many-fields", "", "too many submitted form fields"));
      break;
    }
    if (!Array.isArray(entry) || entry.length !== 2 || typeof entry[0] !== "string") {
      problems.push(problem("invalid-entry", "", "invalid submitted form entry"));
      continue;
    }
    const [name, rawValue] = entry;
    if (typeof rawValue !== "string" || rawValue.length > MAX_FIELD_CHARACTERS) {
      problems.push(problem("invalid-field", name, `invalid value for form field \`${name}\``));
      continue;
    }
    if (seen.has(name)) {
      problems.push(
        problem("duplicate-field", name, `duplicate submitted form field \`${name}\``),
      );
      continue;
    }
    seen.add(name);
    const field = declared.get(name);
    if (!field) {
      problems.push(
        problem("unexpected-field", name, `unexpected submitted form field \`${name}\``),
      );
      continue;
    }
    if (field.required && rawValue.trim() === "") {
      problems.push(problem("missing-field", name, `missing required form field \`${name}\``));
    } else if (rawValue !== "" && !validValue(field.kind, rawValue)) {
      problems.push(problem("invalid-field", name, `invalid value for form field \`${name}\``));
    } else if (field.kind === "secret") {
      secrets.push([name, rawValue]);
    } else {
      values.push({ name, value: rawValue });
    }
  }
  for (const field of action.fields) {
    if (field.required && !seen.has(field.name)) {
      problems.push(
        problem("missing-field", field.name, `missing required form field \`${field.name}\``),
      );
    }
  }
  return problems.length > 0
    ? rejected(problems)
    : Object.freeze({
        ok: true,
        submission: new ProgressiveFormSubmission(values, secrets),
      });
}

export function installProgressiveForms(options = {}) {
  let root = options.root;
  if (!root || typeof root.addEventListener !== "function") {
    throw new TypeError("Glamour progressive forms require an event-capable root");
  }
  const actions = checkedActions(options.actions);
  const fetchImpl = options.fetch ?? globalThis.fetch;
  const FormDataImpl = options.FormData ?? globalThis.FormData;
  const baseUrl = checkedBaseUrl(options.baseUrl ?? globalThis.location?.href);
  const onState = typeof options.onState === "function" ? options.onState : () => {};
  const onLifecycle =
    typeof options.onLifecycle === "function" ? options.onLifecycle : () => {};
  const active = new Map();
  let disposed = false;
  let nextGeneration = 1;

  const publish = (form, action, generation, phase, detail = {}) => {
    const state = Object.freeze({
      phase,
      actionId: action.id,
      generation,
      ...detail,
    });
    form.setAttribute?.("data-glamour-form-state", phase.toLowerCase());
    if (phase === "Submitting") form.setAttribute?.("aria-busy", "true");
    else form.removeAttribute?.("aria-busy");
    onState(state, form);
    onLifecycle(state, action);
    return state;
  };

  const cancel = (form, reason = "superseded") => {
    const current = active.get(form);
    if (!current) return false;
    active.delete(form);
    current.controller.abort();
    publish(form, current.action, current.generation, "Cancelled", { reason });
    return true;
  };

  const submit = async (event) => {
    if (disposed || event?.defaultPrevented === true) return;
    const form = submittedForm(event, root);
    if (!form) return;
    const id = attribute(form, "data-glamour-form");
    const action = actions.get(id);
    if (!action || !enhanceable(form, event?.submitter, action, baseUrl)) return;
    if (typeof fetchImpl !== "function" || typeof FormDataImpl !== "function") return;

    event.preventDefault?.();
    cancel(form);
    const generation = nextGeneration;
    nextGeneration += 1;
    publish(form, action, generation, "Validating");

    let entries;
    try {
      entries = [...new FormDataImpl(form, event?.submitter).entries()];
    } catch {
      publish(form, action, generation, "Failed", {
        reason: "form-data",
        problems: Object.freeze([
          problem("invalid-entry", "", "browser could not read submitted form fields"),
        ]),
      });
      return;
    }
    const decoded = decodeProgressiveForm(action, entries);
    if (!decoded.ok) {
      publish(form, action, generation, "Failed", {
        reason: "validation",
        problems: decoded.problems,
      });
      return;
    }

    const controller = new AbortController();
    active.set(form, { action, controller, generation });
    publish(form, action, generation, "Submitting", {
      values: decoded.submission.values,
    });
    try {
      const request = progressiveRequest(
        action,
        requestEntries(action, entries, decoded.submission),
        baseUrl,
        controller.signal,
      );
      entries = undefined;
      const response = await fetchImpl(request.url, request.init);
      const current = active.get(form);
      if (!current || current.generation !== generation) return;
      active.delete(form);
      const status = checkedResponseStatus(response);
      if (response.ok === true) {
        publish(form, action, generation, "Succeeded", {
          values: decoded.submission.values,
          result: Object.freeze({ status }),
        });
      } else {
        publish(form, action, generation, "Failed", {
          reason: "server",
          values: decoded.submission.values,
          result: Object.freeze({ status }),
        });
      }
    } catch (error) {
      const current = active.get(form);
      if (!current || current.generation !== generation) return;
      active.delete(form);
      if (controller.signal.aborted) {
        publish(form, action, generation, "Cancelled", { reason: "aborted" });
      } else {
        publish(form, action, generation, "Failed", {
          reason: "network",
          values: decoded.submission.values,
        });
      }
    }
  };

  root.addEventListener("submit", submit);
  return Object.freeze({
    cancel,
    rebind(nextRoot) {
      if (disposed) {
        throw new TypeError("disposed Glamour progressive forms cannot be rebound");
      }
      if (!nextRoot || typeof nextRoot.addEventListener !== "function") {
        throw new TypeError("Glamour progressive forms require an event-capable root");
      }
      root.removeEventListener("submit", submit);
      root = nextRoot;
      root.addEventListener("submit", submit);
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      root.removeEventListener("submit", submit);
      for (const form of [...active.keys()]) cancel(form, "disposed");
    },
    get disposed() {
      return disposed;
    },
    get activeCount() {
      return active.size;
    },
  });
}

function requestEntries(action, entries, submission) {
  const secretNames = new Set(
    action.fields.filter(({ kind }) => kind === "secret").map(({ name }) => name),
  );
  return entries.map(([name, value]) => [
    name,
    secretNames.has(name) ? submission.takeSecret(name) : value,
  ]);
}

function checkedActions(actions) {
  if (!Array.isArray(actions)) {
    throw new TypeError("Glamour progressive forms require an action manifest");
  }
  const checked = new Map();
  const inputSchemas = new Set();
  const resultSchemas = new Set();
  for (const action of actions) {
    const schemaProblem = validateAction(action);
    if (
      schemaProblem ||
      !u32Identity(action.inputSchema) ||
      !u32Identity(action.resultSchema) ||
      action.inputSchema === action.resultSchema ||
      inputSchemas.has(action.inputSchema) ||
      resultSchemas.has(action.resultSchema) ||
      checked.has(action.id)
    ) {
      throw new TypeError("Glamour progressive form action manifest is invalid");
    }
    inputSchemas.add(action.inputSchema);
    resultSchemas.add(action.resultSchema);
    checked.set(
      action.id,
      Object.freeze({
        id: action.id,
        method: action.method,
        action: action.action,
        inputSchema: action.inputSchema,
        resultSchema: action.resultSchema,
        fields: Object.freeze(
          action.fields.map((field, ordinal) =>
            Object.freeze({
              ordinal,
              name: field.name,
              label: field.label,
              kind: field.kind,
              required: field.required,
            })
          ),
        ),
      }),
    );
  }
  return checked;
}

function u32Identity(value) {
  return Number.isInteger(value) && value > 0 && value <= 0xffff_ffff;
}

function checkedBaseUrl(value) {
  let parsed;
  try {
    parsed = new URL(value);
  } catch {
    throw new TypeError("Glamour progressive forms require an absolute HTTP(S) base URL");
  }
  if (!["http:", "https:"].includes(parsed.protocol)) {
    throw new TypeError("Glamour progressive forms require an HTTP(S) base URL");
  }
  return parsed;
}

function submittedForm(event, root) {
  const candidate = event?.target;
  if (!candidate || candidate === root) return null;
  if (String(candidate.tagName ?? candidate.tag ?? "").toLowerCase() !== "form") return null;
  if (typeof event.composedPath === "function" && !event.composedPath().includes(root)) return null;
  return candidate;
}

function attribute(node, name) {
  if (typeof node?.getAttribute === "function") return node.getAttribute(name);
  if (node?.attributes instanceof Map) return node.attributes.get(name) ?? null;
  return null;
}

function enhanceable(form, submitter, action, baseUrl) {
  if (
    attribute(form, "method")?.toUpperCase() !== action.method ||
    attribute(form, "action") !== action.action ||
    ![null, "", "_self"].includes(attribute(form, "target")) ||
    (action.method === "POST" &&
      ![null, "", "application/x-www-form-urlencoded"].includes(
        attribute(form, "enctype")?.toLowerCase() ?? null,
      )) ||
    attribute(submitter, "formmethod") !== null ||
    attribute(submitter, "formaction") !== null ||
    attribute(submitter, "formenctype") !== null ||
    attribute(submitter, "formtarget") !== null
  ) {
    return false;
  }
  try {
    return new URL(action.action, baseUrl).origin === baseUrl.origin;
  } catch {
    return false;
  }
}

function progressiveRequest(action, entries, baseUrl, signal) {
  const url = new URL(action.action, baseUrl);
  const body = new URLSearchParams();
  for (const [name, value] of entries) body.append(name, value);
  const init = {
    method: action.method,
    credentials: "same-origin",
    redirect: "manual",
    signal,
    headers: Object.freeze({ Accept: "application/json" }),
  };
  if (action.method === "GET") {
    for (const [name, value] of body) url.searchParams.append(name, value);
  } else {
    init.body = body;
  }
  return { url: url.href, init };
}

function checkedResponseStatus(response) {
  const status = response?.status;
  if (!Number.isInteger(status) || status < 100 || status > 599) {
    throw new TypeError("Glamour progressive action returned an invalid response");
  }
  return status;
}

function validateAction(action) {
  if (
    action === null ||
    typeof action !== "object" ||
    !Array.isArray(action.fields) ||
    action.fields.length > MAX_FIELDS ||
    !["GET", "POST"].includes(action.method) ||
    typeof action.id !== "string" ||
    !/^glamour-form1-[0-9a-f]{64}$/.test(action.id) ||
    typeof action.action !== "string" ||
    !validFormUrl(action.action)
  ) {
    return problem("invalid-schema", "", "invalid progressive form schema");
  }
  const names = new Set();
  for (const field of action.fields) {
    if (
      field === null ||
      typeof field !== "object" ||
      typeof field.name !== "string" ||
      !/^[A-Za-z_][A-Za-z0-9_]*$/.test(field.name) ||
      typeof field.label !== "string" ||
      field.label.length > 1_024 ||
      typeof field.required !== "boolean" ||
      !FIELD_KINDS.has(field.kind) ||
      names.has(field.name)
    ) {
      return problem("invalid-schema", "", "invalid progressive form schema");
    }
    if (action.method === "GET" && field.kind === "secret") {
      return problem("invalid-schema", "", "secret progressive form fields require POST");
    }
    names.add(field.name);
  }
  return undefined;
}

function validFormUrl(value) {
  const normalized = value.trim().toLowerCase();
  if (
    normalized.startsWith("javascript:") ||
    normalized.startsWith("vbscript:") ||
    normalized.startsWith("data:text/html") ||
    normalized.startsWith("//") ||
    /[\n\r\t]/.test(normalized)
  ) {
    return false;
  }
  const colon = normalized.indexOf(":");
  if (colon < 0) {
    return true;
  }
  if (normalized.startsWith("https://") || normalized.startsWith("http://")) {
    return true;
  }
  return ["/", "?", "#"].some((delimiter) => {
    const index = normalized.indexOf(delimiter);
    return index >= 0 && index < colon;
  });
}

function validValue(kind, value) {
  switch (kind) {
    case "text":
    case "secret":
      return true;
    case "email": {
      const pieces = value.split("@");
      return pieces.length === 2 && pieces[0] !== "" && pieces[1].includes(".");
    }
    case "number":
      return /^-?[0-9]+$/.test(value);
    case "checkbox":
      return value === "true" || value === "false" || value === "on";
    default:
      return false;
  }
}

function problem(code, name, message) {
  return Object.freeze({ code, name, message });
}

function rejected(problems) {
  return Object.freeze({ ok: false, problems: Object.freeze(problems) });
}

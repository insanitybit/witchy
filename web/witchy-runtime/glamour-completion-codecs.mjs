// Closed RFC-0108 production completion codecs. Descriptor semantics are
// compiler-authenticated manifest data; result-schema integers remain data and
// never select executable JavaScript.

import { CompletionStatus } from "./glamour-protocol.mjs";

const encoder = new TextEncoder();
const MAX_U32 = 0xffff_ffff;

function fail(message) {
  throw new Error(`glamour completion codec: ${message}`);
}

function checkedLimit(value) {
  if (!Number.isInteger(value) || value < 0 || value > MAX_U32) {
    fail("payload limit is invalid");
  }
  return value;
}

function textRecord(tag, text, leading = []) {
  const body = encoder.encode(text);
  const headerBytes = 8 + leading.length * 4;
  const output = new Uint8Array(headerBytes + body.byteLength);
  const view = new DataView(output.buffer);
  view.setUint8(0, tag);
  for (let index = 0; index < leading.length; index += 1) {
    view.setUint32(4 + index * 4, leading[index], true);
  }
  view.setUint32(4 + leading.length * 4, body.byteLength, true);
  output.set(body, headerBytes);
  return output;
}

function failurePayload(value, tag = 2) {
  return textRecord(tag, typeof value === "string" ? value : "");
}

function emptyRecord(tag) {
  const output = new Uint8Array(4);
  new DataView(output.buffer).setUint8(0, tag);
  return output;
}

function boundedResult(status, payload, maximum, fallback) {
  if (payload.byteLength <= maximum) return Object.freeze({ status, payload });
  const replacement = fallback();
  if (replacement.byteLength > maximum) {
    fail("payload limit cannot hold the canonical failure record");
  }
  return Object.freeze({ status: CompletionStatus.Error, payload: replacement });
}

function compatibilityResult(status, value, maximum) {
  const payload = encoder.encode(String(value));
  if (payload.byteLength <= maximum) return Object.freeze({ status, payload });
  return Object.freeze({ status: CompletionStatus.Error, payload: new Uint8Array() });
}

function unitResult(status) {
  if (status !== CompletionStatus.Ok) {
    fail("unit completion cannot carry an error status");
  }
  return Object.freeze({ status, payload: new Uint8Array() });
}

function httpResult(status, value, maximum) {
  if (status === CompletionStatus.Error) {
    return boundedResult(status, failurePayload(value), maximum, () => failurePayload(""));
  }
  if (
    !value ||
    typeof value !== "object" ||
    !Number.isInteger(value.status) ||
    value.status < 100 ||
    value.status > 599 ||
    typeof value.body !== "string"
  ) {
    return boundedResult(
      CompletionStatus.Error,
      failurePayload(""),
      maximum,
      () => failurePayload(""),
    );
  }
  return boundedResult(
    status,
    textRecord(1, value.body, [value.status]),
    maximum,
    () => failurePayload(""),
  );
}

function navigationResult(status, value, maximum) {
  if (status === CompletionStatus.Error) {
    return boundedResult(status, failurePayload(value), maximum, () => failurePayload(""));
  }
  const path = typeof value === "string" ? value : value?.path;
  if (typeof path !== "string") {
    return boundedResult(
      CompletionStatus.Error,
      failurePayload(""),
      maximum,
      () => failurePayload(""),
    );
  }
  return boundedResult(status, textRecord(1, path), maximum, () => failurePayload(""));
}

function portResult(status, value, maximum) {
  if (status === CompletionStatus.Error) {
    return boundedResult(status, failurePayload(value), maximum, () => failurePayload(""));
  }
  if (typeof value !== "string") {
    return boundedResult(
      CompletionStatus.Error,
      failurePayload(""),
      maximum,
      () => failurePayload(""),
    );
  }
  return boundedResult(status, textRecord(1, value), maximum, () => failurePayload(""));
}

function storageResult(semantic, status, value, maximum) {
  if (status === CompletionStatus.Error) {
    return boundedResult(status, failurePayload(value, 5), maximum, () => failurePayload("", 5));
  }
  let payload;
  if (semantic === "storage-get") {
    if (value?.kind === "missing") payload = emptyRecord(1);
    else if (value?.kind === "value" && typeof value.value === "string") {
      payload = textRecord(2, value.value);
    } else {
      return boundedResult(
        CompletionStatus.Error,
        failurePayload("", 5),
        maximum,
        () => failurePayload("", 5),
      );
    }
  } else {
    payload = emptyRecord(semantic === "storage-set" ? 3 : 4);
  }
  return boundedResult(status, payload, maximum, () => failurePayload("", 5));
}

function workerResult(status, value, maximum) {
  const text = typeof value === "string" ? value : "worker failed";
  const bounded = status === CompletionStatus.Ok ? text : text.slice(0, 128);
  return boundedResult(
    status,
    encoder.encode(bounded),
    maximum,
    () => encoder.encode("worker result exceeded its authenticated limit"),
  );
}

function hostPortResult(status, value, maximum) {
  const text = typeof value === "string" ? value : "host port failed";
  const bounded = status === CompletionStatus.Ok ? text : text.slice(0, 128);
  return boundedResult(
    status,
    encoder.encode(bounded),
    maximum,
    () => encoder.encode("host port result exceeded its authenticated limit"),
  );
}

export function encodeCompletionResult({ descriptor, status, value, maxBytes }) {
  const maximum = checkedLimit(maxBytes);
  const semantic = descriptor?.semantic;
  if (semantic === undefined || semantic === null) {
    return compatibilityResult(status, value, maximum);
  }
  switch (semantic) {
    case "timer":
    case "interval":
      return unitResult(status);
    case "http":
      return httpResult(status, value, maximum);
    case "navigation":
      return navigationResult(status, value, maximum);
    case "port":
    case "secret":
      return portResult(status, value, maximum);
    case "storage-get":
    case "storage-set":
    case "storage-remove":
      return storageResult(semantic, status, value, maximum);
    case "worker":
      return workerResult(status, value, maximum);
    case "host-port":
      return hostPortResult(status, value, maximum);
    default:
      fail(`descriptor semantic ${JSON.stringify(semantic)} has no production codec`);
  }
}

// Closed Web Storage effect host for compiler-authenticated Glamour descriptors.

const encoder = new TextEncoder();

function fail(message) {
  throw new Error(`glamour storage: ${message}`);
}

function requestFields(source, count) {
  if (typeof source !== "string") fail("request is not text");
  const fields = [];
  let cursor = 0;
  for (let index = 0; index < count; index += 1) {
    const colon = source.indexOf(":", cursor);
    if (colon < cursor) fail("request is malformed");
    const length = Number(source.slice(cursor, colon));
    if (!Number.isSafeInteger(length) || length < 0) fail("field length is invalid");
    const start = colon + 1;
    let end = start;
    while (end <= source.length && encoder.encode(source.slice(start, end)).byteLength < length) {
      end += 1;
    }
    if (encoder.encode(source.slice(start, end)).byteLength !== length) fail("field is truncated");
    fields.push(source.slice(start, end));
    cursor = end;
  }
  if (cursor !== source.length) fail("request has trailing data");
  return fields;
}

export function createStorageEffectHandler({
  artifact,
  local = () => globalThis.localStorage,
  session = () => globalThis.sessionStorage,
} = {}) {
  if (!artifact || typeof artifact !== "object" || Array.isArray(artifact)) {
    fail("artifact is missing");
  }
  if (typeof artifact.grantDigest !== "string" || !/^[0-9a-f]{64}$/.test(artifact.grantDigest)) {
    fail("artifact grant digest is invalid");
  }
  return function storageEffect({ request, descriptor }) {
    const published = artifact.effectDescriptors?.[String(descriptor)];
    const semantic = published?.semantic;
    const policy = published?.policy;
    const expected = semantic === "storage-set" ? 6 : 5;
    const fields = requestFields(request, expected);
    const [provider, namespace, keyPrefix, maximumText, key, value] = fields;
    const maximum = Number(maximumText);
    const granted = policy?.kind === "storage" && policy.provider === provider &&
      policy.namespace === namespace && policy.keyPrefix === keyPrefix &&
      policy.maxValueBytes === maximum;
    if (!granted || !new Set(["storage-get", "storage-set", "storage-remove"]).has(semantic) ||
        !Number.isSafeInteger(maximum) || maximum < 0 || maximum > 65_536 ||
        key.includes("\0") || encoder.encode(key).byteLength > 256 || !key.startsWith(keyPrefix)) {
      fail("request exceeds its build-authenticated policy");
    }
    if (semantic === "storage-set" && encoder.encode(value).byteLength > maximum) {
      fail("value exceeds its build-authenticated policy");
    }
    const browserStorage = provider === "session" ? session() : provider === "local" ? local() : null;
    if (!browserStorage) fail("provider is unavailable");
    const physicalKey = `witchy:${artifact.grantDigest}:${namespace}:${key}`;
    if (semantic === "storage-get") {
      const stored = browserStorage.getItem(physicalKey);
      if (stored === null) return { kind: "missing" };
      if (typeof stored !== "string" || encoder.encode(stored).byteLength > maximum) {
        fail("stored value exceeds its current policy");
      }
      return { kind: "value", value: stored };
    }
    if (semantic === "storage-set") browserStorage.setItem(physicalKey, value);
    else browserStorage.removeItem(physicalKey);
    return undefined;
  };
}

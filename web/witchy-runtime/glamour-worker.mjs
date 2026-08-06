// Closed browser-worker host for compiler-authenticated Glamour tasks.

const encoder = new TextEncoder();

function fail(message) {
  throw new Error(`glamour worker: ${message}`);
}

function exactPolicy(artifact, descriptor) {
  const policy = artifact?.effectDescriptors?.[String(descriptor)]?.policy;
  if (
    policy?.kind !== "worker" ||
    typeof policy.name !== "string" ||
    typeof policy.artifact !== "string" ||
    typeof policy.url !== "string" ||
    policy.export !== "__export_export_glamour_worker_execute" ||
    !Number.isInteger(policy.maxRequestBytes) || policy.maxRequestBytes < 1 || policy.maxRequestBytes > 65_536 ||
    !Number.isInteger(policy.maxResultBytes) || policy.maxResultBytes < 1 || policy.maxResultBytes > 65_536 ||
    !Number.isInteger(policy.maxConcurrency) || policy.maxConcurrency < 1 || policy.maxConcurrency > 16 ||
    !Number.isInteger(policy.timeoutMs) || policy.timeoutMs < 1 || policy.timeoutMs > 300_000
  ) {
    fail(`descriptor ${descriptor} has no closed worker policy`);
  }
  return policy;
}

export function createWorkerEffectHandler({
  artifact,
  shellUrl,
  resolveUrl,
  WorkerImpl = globalThis.Worker,
  setTimer = globalThis.setTimeout,
  clearTimer = globalThis.clearTimeout,
}) {
  if (typeof WorkerImpl !== "function") fail("Worker is unavailable");
  if (typeof shellUrl !== "string" || shellUrl === "") fail("worker shell URL is absent");
  if (typeof resolveUrl !== "function") fail("worker URL resolver is absent");
  const active = new Map();

  return function workerEffect({ request, signal, descriptor }) {
    const policy = exactPolicy(artifact, descriptor);
    if (typeof request !== "string" || encoder.encode(request).byteLength > policy.maxRequestBytes) {
      return Promise.reject(new Error("worker request exceeds its build-authenticated policy"));
    }
    const key = `${policy.name}\u0000${policy.artifact}`;
    const count = active.get(key) ?? 0;
    if (count >= policy.maxConcurrency) {
      return Promise.reject(new Error("worker concurrency exceeds its build-authenticated policy"));
    }
    active.set(key, count + 1);
    const worker = new WorkerImpl(shellUrl, { type: "module", name: `glamour-${policy.name}` });
    let settled = false;
    let timer = null;
    let rejectPromise;
    const finish = () => {
      if (settled) return false;
      settled = true;
      if (timer !== null) clearTimer(timer);
      worker.terminate();
      const remaining = (active.get(key) ?? 1) - 1;
      if (remaining <= 0) active.delete(key);
      else active.set(key, remaining);
      return true;
    };
    const promise = new Promise((resolve, reject) => {
      rejectPromise = reject;
      worker.onmessage = (event) => {
        const result = event?.data?.result;
        if (!finish()) return;
        if (typeof result !== "string" || encoder.encode(result).byteLength > policy.maxResultBytes) {
          reject(new Error("worker result exceeds its build-authenticated policy"));
          return;
        }
        resolve(result);
      };
      worker.onerror = () => {
        if (finish()) reject(new Error("worker failed"));
      };
      timer = setTimer(() => {
        if (finish()) reject(new Error("worker timed out"));
      }, policy.timeoutMs);
      worker.postMessage(Object.freeze({
        wasmUrl: String(resolveUrl(policy.url)),
        artifact: policy.artifact,
        exportName: policy.export,
        request,
        maxRequestBytes: policy.maxRequestBytes,
        maxResultBytes: policy.maxResultBytes,
      }));
    });
    const cancel = () => {
      if (finish()) rejectPromise?.(new Error("worker cancelled"));
    };
    signal?.addEventListener?.("abort", cancel, { once: true });
    return { promise, cancel };
  };
}

import assert from "node:assert/strict";
import { createWorkerEffectHandler } from "./glamour-worker.mjs";

const artifact = {
  effectDescriptors: {
    7: {
      semantic: "worker",
      policy: {
        kind: "worker",
        name: "double",
        artifact: `glamour-worker1-${"a".repeat(64)}`,
        url: "/assets/worker-aaaaaaaaaaaaaaaa.wasm",
        export: "__export_export_glamour_worker_execute",
        maxRequestBytes: 32,
        maxResultBytes: 32,
        maxConcurrency: 1,
        timeoutMs: 100,
      },
    },
  },
};

class FakeWorker {
  static instances = [];
  constructor(url, options) {
    this.url = url;
    this.options = options;
    this.terminated = false;
    FakeWorker.instances.push(this);
  }
  postMessage(message) { this.message = message; }
  terminate() { this.terminated = true; }
}

const handler = createWorkerEffectHandler({
  artifact,
  shellUrl: "https://example.test/assets/glamour-worker-shell.mjs",
  resolveUrl: (url) => new URL(url, "https://example.test/"),
  WorkerImpl: FakeWorker,
});

const first = handler({ request: "request", descriptor: 7, signal: new AbortController().signal });
assert.equal(FakeWorker.instances[0].options.type, "module");
assert.equal(FakeWorker.instances[0].message.exportName, "__export_export_glamour_worker_execute");
await assert.rejects(
  handler({ request: "second", descriptor: 7, signal: new AbortController().signal }),
  /concurrency/,
);
FakeWorker.instances[0].onmessage({ data: { result: "result" } });
assert.equal(await first.promise, "result");
assert.equal(FakeWorker.instances[0].terminated, true);

const controller = new AbortController();
const cancelled = handler({ request: "request", descriptor: 7, signal: controller.signal });
controller.abort();
await assert.rejects(cancelled.promise, /cancelled/);
assert.equal(FakeWorker.instances[1].terminated, true);

assert.throws(
  () => createWorkerEffectHandler({ artifact: { effectDescriptors: {} }, shellUrl: "x", resolveUrl: String, WorkerImpl: FakeWorker })({ request: "x", descriptor: 7 }),
  /closed worker policy/,
);

console.log("glamour worker host: ok");

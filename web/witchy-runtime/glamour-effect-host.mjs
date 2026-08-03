// Host-custodied asynchronous work for the RFC-0108 optimized runtime.
//
// Descriptors select only handlers granted by the embedding shell. Wasm owns
// stable numeric identities and request data; this module owns generations,
// cancellation handles, abort signals, and stale-callback suppression.

import {
  CompletionSource,
  CompletionStatus,
} from "./glamour-protocol.mjs";

function fail(message) {
  throw new Error(`glamour effects: ${message}`);
}

function asHandlerMap(value) {
  return value instanceof Map ? value : new Map(Object.entries(value || {}));
}

export function createEffectHost({
  effectDescriptors,
  subscriptionDescriptors,
  effectHandlers,
  subscriptionHandlers,
  complete,
  observeLifecycle,
}) {
  const effects = new Map();
  const effectKeys = new Map();
  const subscriptions = new Map();
  const effectHandlerMap = asHandlerMap(effectHandlers);
  const subscriptionHandlerMap = asHandlerMap(subscriptionHandlers);
  let nextGeneration = 1;
  let disposed = false;
  const notifyLifecycle = (event) => {
    if (typeof observeLifecycle !== "function") return;
    try {
      observeLifecycle(Object.freeze(event));
    } catch {
      // Development observation is diagnostic-only and cannot affect host work.
    }
  };

  const resolveDescriptor = (descriptors, handlers, id, label) => {
    const descriptor = descriptors.get(id);
    if (
      !descriptor ||
      typeof descriptor !== "object" ||
      !Number.isInteger(descriptor.resultSchema) ||
      descriptor.resultSchema <= 0 ||
      descriptor.resultSchema > 0xffff_ffff ||
      !Number.isInteger(descriptor.ownerScope) ||
      descriptor.ownerScope <= 0 ||
      descriptor.ownerScope > 0xffff_ffff
    ) {
      fail(`${label} descriptor ${id} is malformed`);
    }
    const handler =
      handlers.get(descriptor.handler) ?? handlers.get(String(descriptor.handler));
    if (typeof handler !== "function") {
      fail(`${label} descriptor ${id} has no granted host handler`);
    }
    return { descriptor, handler };
  };

  const freshGeneration = () => {
    if (nextGeneration > 0xffff_ffff) fail("host work generation space is exhausted");
    return nextGeneration++;
  };

  const cancelEntry = (entry) => {
    let failure = null;
    try {
      entry.controller.abort();
    } catch (error) {
      failure = error;
    }
    try {
      if (typeof entry.cancel === "function") entry.cancel();
    } catch (error) {
      failure ??= error;
    }
    if (failure && !disposed) throw failure;
  };

  const retireEffect = (instance, cancel) => {
    const entry = effects.get(instance);
    if (!entry) return;
    effects.delete(instance);
    if (
      entry.cancellationKey !== 0 &&
      effectKeys.get(entry.cancellationKey) === instance
    ) {
      effectKeys.delete(entry.cancellationKey);
    }
    if (cancel) {
      cancelEntry(entry);
      notifyLifecycle({
        kind: "effect",
        phase: "cancelled",
        instance,
        descriptor: entry.descriptor,
        generation: entry.generation,
      });
    }
  };

  const cancelEffectInstance = (instance) => retireEffect(instance, true);

  const cancelEffectKey = (key) => {
    const instance = effectKeys.get(key);
    if (instance !== undefined) cancelEffectInstance(instance);
  };

  const cancelSubscription = (subscription) => {
    const entry = subscriptions.get(subscription);
    if (!entry) return;
    subscriptions.delete(subscription);
    cancelEntry(entry);
    notifyLifecycle({
      kind: "subscription",
      phase: "cancelled",
      instance: subscription,
      descriptor: entry.descriptor,
      generation: entry.generation,
    });
  };

  const startEffect = (operation) => {
    const { descriptor, handler } = resolveDescriptor(
      effectDescriptors,
      effectHandlerMap,
      operation.descriptor,
      "effect",
    );
    if (effects.has(operation.instance)) {
      fail(`effect instance ${operation.instance} is already live`);
    }
    if (operation.cancellationKey !== 0) cancelEffectKey(operation.cancellationKey);
    const generation = freshGeneration();
    const controller = new AbortController();
    const entry = {
      generation,
      cancellationKey: operation.cancellationKey,
      descriptor: operation.descriptor,
      controller,
      cancel: null,
    };
    effects.set(operation.instance, entry);
    if (operation.cancellationKey !== 0) {
      effectKeys.set(operation.cancellationKey, operation.instance);
    }
    notifyLifecycle({
      kind: "effect",
      phase: "started",
      instance: operation.instance,
      descriptor: operation.descriptor,
      generation,
    });
    let result;
    try {
      result = handler({
        request: operation.request,
        signal: controller.signal,
        instance: operation.instance,
        descriptor: operation.descriptor,
      });
      if (result && typeof result === "object" && "promise" in result) {
        entry.cancel = typeof result.cancel === "function" ? result.cancel : null;
        result = result.promise;
      }
    } catch {
      result = Promise.reject(new Error("host effect failed"));
    }
    Promise.resolve(result).then(
      (value) => {
        if (effects.get(operation.instance) !== entry) return;
        retireEffect(operation.instance, false);
        notifyLifecycle({
          kind: "effect",
          phase: "completed",
          status: "ok",
          instance: operation.instance,
          descriptor: operation.descriptor,
          generation,
        });
        complete({
          source: CompletionSource.Effect,
          instance: operation.instance,
          generation,
          descriptor: operation.descriptor,
          resultSchema: descriptor.resultSchema,
          status: CompletionStatus.Ok,
          value,
        });
      },
      () => {
        if (effects.get(operation.instance) !== entry) return;
        retireEffect(operation.instance, false);
        notifyLifecycle({
          kind: "effect",
          phase: "completed",
          status: "error",
          instance: operation.instance,
          descriptor: operation.descriptor,
          generation,
        });
        complete({
          source: CompletionSource.Effect,
          instance: operation.instance,
          generation,
          descriptor: operation.descriptor,
          resultSchema: descriptor.resultSchema,
          status: CompletionStatus.Error,
          value: "",
        });
      },
    );
  };

  const syncSubscription = (operation) => {
    const { descriptor, handler } = resolveDescriptor(
      subscriptionDescriptors,
      subscriptionHandlerMap,
      operation.descriptor,
      "subscription",
    );
    const fingerprint = `${operation.descriptor}\u0000${operation.request}`;
    const current = subscriptions.get(operation.subscription);
    if (current?.fingerprint === fingerprint) return;
    if (current) cancelSubscription(operation.subscription);
    const generation = freshGeneration();
    const controller = new AbortController();
    const entry = {
      fingerprint,
      generation,
      descriptor: operation.descriptor,
      controller,
      cancel: null,
    };
    subscriptions.set(operation.subscription, entry);
    notifyLifecycle({
      kind: "subscription",
      phase: "started",
      instance: operation.subscription,
      descriptor: operation.descriptor,
      generation,
    });
    const emit = (value) => {
      queueMicrotask(() => {
        if (subscriptions.get(operation.subscription) !== entry) return;
        notifyLifecycle({
          kind: "subscription",
          phase: "emitted",
          status: "ok",
          instance: operation.subscription,
          descriptor: operation.descriptor,
          generation,
        });
        complete({
          source: CompletionSource.Subscription,
          instance: operation.subscription,
          generation,
          descriptor: operation.descriptor,
          resultSchema: descriptor.resultSchema,
          status: CompletionStatus.Ok,
          value,
        });
      });
    };
    try {
      const result = handler({
        request: operation.request,
        signal: controller.signal,
        subscription: operation.subscription,
        descriptor: operation.descriptor,
        emit,
      });
      entry.cancel =
        typeof result === "function"
          ? result
          : typeof result?.cancel === "function"
            ? result.cancel
            : null;
    } catch (error) {
      cancelSubscription(operation.subscription);
      throw error;
    }
  };

  return Object.freeze({
    validateEffectDescriptor(id) {
      resolveDescriptor(effectDescriptors, effectHandlerMap, id, "effect");
    },
    validateSubscriptionDescriptor(id) {
      resolveDescriptor(
        subscriptionDescriptors,
        subscriptionHandlerMap,
        id,
        "subscription",
      );
    },
    hasEffectInstance(instance) {
      return effects.has(instance);
    },
    startEffect,
    cancelEffectKey,
    syncSubscription,
    cancelSubscription,
    dispose() {
      if (disposed) return;
      disposed = true;
      for (const instance of [...effects.keys()]) cancelEffectInstance(instance);
      for (const subscription of [...subscriptions.keys()]) {
        cancelSubscription(subscription);
      }
      effectKeys.clear();
    },
    get activeEffectCount() {
      return effects.size;
    },
    get activeSubscriptionCount() {
      return subscriptions.size;
    },
  });
}

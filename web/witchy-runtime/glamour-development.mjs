// Development-only state-preserving swap coordinator.
//
// Production bundles omit this module. The coordinator keeps the old
// application live until a detached candidate has authenticated and restored
// the compiler-owned snapshot successfully.

function sameIdentity(expected, actual, label) {
  if (typeof expected !== "string" || expected !== actual) {
    throw new Error(`Glamour development swap rejected: ${label} does not match`);
  }
}

export function installDevelopmentSwap({
  application: initialApplication,
  root,
  manifest: initialManifest,
  mountOptimized,
  instantiateOptions,
}) {
  let application = initialApplication;
  let manifest = initialManifest;
  let swapping = false;

  const performSwap = async (next) => {
    if (next?.decision !== "swap") {
      throw new Error(`Glamour development swap rejected: ${next?.reason || "reload required"}`);
    }
    const current = manifest.development;
    if (!current || !application.developmentMetadata) {
      throw new Error("Glamour development swap rejected: current compiler metadata is absent");
    }
    if (![1, 2].includes(current.snapshotFormat) || current.maxSnapshotBytes <= 0) {
      throw new Error("Glamour development swap rejected: current model is reload-only");
    }
    for (const [eventKey, manifestKey, label] of [
      ["applicationIdentity", "applicationIdentity", "application identity"],
      ["authorizationSchema", "authorizationSchema", "authorization schema"],
      ["templateSchema", "templateSchema", "template schema"],
    ]) {
      sameIdentity(next[eventKey], current[manifestKey], label);
    }
    const migratesModel = next.modelSchema !== current.modelSchema;
    if (
      migratesModel &&
      (!Array.isArray(next.migrationSchemas) ||
        !next.migrationSchemas.includes(current.modelSchema))
    ) {
      throw new Error("Glamour development swap rejected: model schema does not match");
    }
    if (
      next.snapshotFormat !== current.snapshotFormat ||
      next.maxSnapshotBytes !== current.maxSnapshotBytes
    ) {
      throw new Error("Glamour development swap rejected: snapshot contract changed");
    }
    sameIdentity(
      current.modelSchema,
      application.developmentMetadata.modelSchema,
      "live model schema",
    );
    sameIdentity(
      current.authorizationSchema,
      application.developmentMetadata.authorizationSchema,
      "live authorization schema",
    );
    const snapshot = application.snapshot();
    if (snapshot.byteLength > current.maxSnapshotBytes) {
      throw new Error("Glamour development swap rejected: snapshot exceeds its manifest limit");
    }

    const [wasmResponse, manifestResponse] = await Promise.all([
      fetch(next.wasm, { credentials: "same-origin", cache: "no-store" }),
      fetch(next.manifest, { credentials: "same-origin", cache: "no-store" }),
    ]);
    if (!wasmResponse.ok || !manifestResponse.ok) {
      throw new Error("Glamour development swap rejected: candidate artifacts are unavailable");
    }
    const candidateManifest = await manifestResponse.json();
    const candidateContract = candidateManifest.development;
    if (!candidateContract) {
      throw new Error("Glamour development swap rejected: candidate metadata is absent");
    }
    for (const [eventKey, manifestKey, label] of [
      ["applicationIdentity", "applicationIdentity", "candidate application identity"],
      ["modelSchema", "modelSchema", "candidate model schema"],
      ["authorizationSchema", "authorizationSchema", "candidate authorization schema"],
      ["templateSchema", "templateSchema", "candidate template schema"],
    ]) {
      sameIdentity(next[eventKey], candidateContract[manifestKey], label);
    }
    if (
      migratesModel &&
      (!Array.isArray(candidateContract.migrationSchemas) ||
        !candidateContract.migrationSchemas.includes(current.modelSchema))
    ) {
      throw new Error("Glamour development swap rejected: candidate migration is absent");
    }
    if (
      candidateContract.snapshotFormat !== next.snapshotFormat ||
      candidateContract.maxSnapshotBytes !== next.maxSnapshotBytes
    ) {
      throw new Error("Glamour development swap rejected: candidate snapshot contract changed");
    }

    const detached = document.createElement("div");
    let candidate;
    try {
      candidate = await mountOptimized(await wasmResponse.arrayBuffer(), detached, {
        manifest: candidateManifest,
        restoreSnapshot: snapshot,
        deferActivation: true,
        instantiateOptions,
      });
      sameIdentity(
        candidateContract.modelSchema,
        candidate.developmentMetadata?.modelSchema,
        "candidate runtime model schema",
      );
      sameIdentity(
        candidateContract.authorizationSchema,
        candidate.developmentMetadata?.authorizationSchema,
        "candidate runtime authorization schema",
      );
    } catch (error) {
      candidate?.dispose();
      throw error;
    }

    application.dispose();
    candidate.activate(root);
    application = candidate;
    manifest = candidateManifest;
    return Object.freeze({
      buildId: next.buildId,
      modelSchema: candidate.developmentMetadata.modelSchema,
      restoredBytes: snapshot.byteLength,
    });
  };

  const swap = async (next) => {
    if (swapping) {
      throw new Error("Glamour development swap rejected: another candidate is pending");
    }
    swapping = true;
    try {
      return await performSwap(next);
    } finally {
      swapping = false;
    }
  };

  const bridge = Object.freeze({
    swap,
    inspect() {
      return typeof application.inspectDevelopment === "function"
        ? application.inspectDevelopment()
        : null;
    },
  });
  Object.defineProperty(globalThis, "__WITCHY_GLAMOUR_DEV__", {
    value: bridge,
    configurable: false,
    enumerable: false,
    writable: false,
  });
  return bridge;
}

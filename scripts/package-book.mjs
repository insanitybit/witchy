#!/usr/bin/env node

import { createHash } from "node:crypto";
import {
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join, relative, resolve, sep } from "node:path";

const output = resolve(process.argv[2] || "dist");
const manifestPath = join(output, "witchy-web-manifest.json");
const reportPath = join(output, "witchy-build-report.json");
const sbomPath = join(output, "witchy-sbom.cdx.json");

function deploymentBase() {
  let value = process.env.WITCHY_BOOK_BASE;
  if (!value && process.env.GITHUB_ACTIONS === "true" && process.env.GITHUB_REPOSITORY) {
    value = `/${process.env.GITHUB_REPOSITORY.split("/").at(-1)}/`;
  }
  value ||= "/";
  if (!/^\/(?:[A-Za-z0-9._-]+\/)*$/.test(value) || value.includes("../")) {
    throw new Error(`invalid WITCHY_BOOK_BASE ${JSON.stringify(value)}; expected a canonical absolute directory path`);
  }
  return value;
}

const json = (path) => JSON.parse(readFileSync(path, "utf8"));
const writeJson = (path, value) => writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const slashPath = (path) => path.split(sep).join("/");
const filesUnder = (root) => readdirSync(root, { withFileTypes: true }).flatMap((entry) => {
  const path = join(root, entry.name);
  return entry.isDirectory() ? filesUnder(path) : [path];
});
const artifactRecords = () => filesUnder(output)
  .filter((path) => path !== reportPath)
  .map((path) => {
    const bytes = readFileSync(path);
    return {
      path: slashPath(relative(output, path)),
      bytes: bytes.length,
      sha256: sha256(bytes),
    };
  })
  .sort((left, right) => left.path < right.path ? -1 : left.path > right.path ? 1 : 0);

const manifest = json(manifestPath);
const basePath = deploymentBase();
const routes = [];
for (const route of manifest.routes) {
  const path = join(output, route.file);
  let source = readFileSync(path, "utf8").replace(/<[^>]+>/g, (tag) =>
    tag.replace(/\b(href|src|action)="\/(?!\/)/g, `$1="${basePath}`));
  if (source.includes('data-witchy-runnable="1"')
      || source.includes('data-glamour-slot-kind="witchy-runnable"')) {
    if (!source.includes("</body>")) throw new Error(`runnable route ${route.path} has no body`);
    if (source.includes("data-witchy-runnables")) throw new Error(`runnable route ${route.path} already has a loader`);
    // The bundle's recorded contract is `parentContentSecurityPolicy: "omitted"`
    // on runnable routes: an `about:srcdoc` frame INHERITS the parent page's
    // CSP, and a parent policy can never allowlist the sandbox frame's per-run
    // random script nonce (or `trusted-types` its srcdoc mount), so any parent
    // CSP bricks the opaque-frame runner — the reader sees
    // "This document requires 'TrustedHTML' assignment" and dead Run buttons.
    // The publisher stamps its strict meta CSP into every page; strip it from
    // exactly the routes this packager makes runnable. Isolation on these
    // routes rides on the frame's own capability-derived meta CSP plus
    // `sandbox="allow-scripts"` (recorded as `sandboxContentSecurityPolicy:
    // "capability-derived"`); non-runnable routes keep the strict parent CSP
    // and ship zero script.
    const metaCsp = /<meta http-equiv="Content-Security-Policy" content="[^"]*">/;
    if (!metaCsp.test(source)) throw new Error(`runnable route ${route.path} has no publisher meta CSP to omit`);
    source = source.replace(metaCsp, "");
    const loader = `<script type="module" src="${basePath}docs-static-boot.js" data-witchy-runnables></script>`;
    source = source.replace("</body>", `${loader}</body>`);
    routes.push(route.path);
  }
  writeFileSync(path, source);
}

const hostPaths = [
  "docs-asset-url.js",
  "docs-run-options.js",
  "docs-static-boot.js",
  "examples.json",
  "fixture-showcase/fixture_showcase.witchy",
  "fixture-showcase/release.fixture.json",
  "wasm-fetch.js",
  "witchy-cell-frame.js",
  "witchy-cell-sandbox.js",
  "witchy-highlight.js",
  "witchy-host.js",
  "witchy-runnable.js",
  "witchy-runtime/witchy-runtime.mjs",
  "witchy.wasm",
];
for (const path of hostPaths) {
  if (!statSync(join(output, path)).isFile()) throw new Error(`book host artifact ${path} is missing`);
}
const hostArtifacts = hostPaths.map((path) => {
  const bytes = readFileSync(join(output, path));
  return { path, bytes: bytes.length, sha256: sha256(bytes) };
});
const bundleIdentity = sha256(Buffer.from(JSON.stringify({
  buildIdentity: manifest.buildIdentity,
  basePath,
  hostArtifacts,
  routes,
})));
const hostFacilities = {
  schema: "witchy.book.runnable-host.v1",
  bundleIdentity,
  basePath,
  compiler: `${basePath}witchy.wasm`,
  loader: `${basePath}docs-static-boot.js`,
  isolation: "opaque-frame",
  parentContentSecurityPolicy: "omitted",
  sandboxContentSecurityPolicy: "capability-derived",
  routes,
  artifacts: hostArtifacts,
};
manifest.bundleIdentity = bundleIdentity;
manifest.hostFacilities = hostFacilities;
writeJson(manifestPath, manifest);

const sbom = json(sbomPath);
const hostReferences = hostArtifacts.map((artifact) => `witchy-book-host:${artifact.path}`);
for (const [index, artifact] of hostArtifacts.entries()) {
  sbom.components.push({
    "bom-ref": hostReferences[index],
    type: "file",
    name: artifact.path,
    hashes: [{ alg: "SHA-256", content: artifact.sha256 }],
    properties: [{ name: "witchy.web.host-facility", value: "runnable-cell" }],
  });
}
sbom.dependencies[0].dependsOn.push(...hostReferences);
sbom.dependencies.push(...hostReferences.map((ref) => ({ ref, dependsOn: [] })));
writeJson(sbomPath, sbom);

writeFileSync(join(output, "_headers"), `/*
  Referrer-Policy: no-referrer
  X-Content-Type-Options: nosniff
  Permissions-Policy: camera=(), microphone=(), geolocation=()

/*.wasm
  Content-Type: application/wasm

/assets/*.wasm
  Content-Type: application/wasm
  Cache-Control: public, max-age=31536000, immutable

/assets/*
  Cache-Control: public, max-age=31536000, immutable
`);

const report = json(reportPath);
report.bundleIdentity = bundleIdentity;
report.hostFacilities = {
  schema: hostFacilities.schema,
  basePath: hostFacilities.basePath,
  isolation: hostFacilities.isolation,
  parentContentSecurityPolicy: hostFacilities.parentContentSecurityPolicy,
  sandboxContentSecurityPolicy: hostFacilities.sandboxContentSecurityPolicy,
  routes: routes.length,
  artifacts: hostArtifacts.length,
};
report.artifacts = artifactRecords();
writeJson(reportPath, report);

const recorded = JSON.stringify(report.artifacts);
const emitted = JSON.stringify(artifactRecords());
if (recorded !== emitted) throw new Error("book report artifact graph does not match emitted files");

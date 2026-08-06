#!/usr/bin/env node
// The deployable Witchy book is native static HTML plus compiler-lowered
// interactive regions. Runnable code fences are a separately recorded host
// facility and never turn the whole page back into a client-rendered Wasm app.
//
// Usage: node web/witchy-runtime/glamour-docs-bundle.test.mjs [path/to/witchy]

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const BIN = process.argv[2] || resolve(process.cwd(), "target/debug/witchy");
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const SCRIPT = join(REPO, "scripts/build-docs.sh");
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const json = (path) => JSON.parse(readFileSync(path, "utf8"));
const filesUnder = (root) => readdirSync(root, { withFileTypes: true }).flatMap((entry) => {
  const path = join(root, entry.name);
  return entry.isDirectory() ? filesUnder(path) : [path];
});

let failures = 0;
const ok = (condition, message) => {
  console.log(`  ${condition ? "ok" : "FAIL"}: ${message}`);
  if (!condition) failures++;
};

const scratch = mkdtempSync(join(tmpdir(), "witchy-book-bundle-"));
try {
  const missingCompiler = join(scratch, "missing-compiler.wasm");
  let rejectedMissingCompiler = false;
  try {
    execFileSync("bash", [SCRIPT, join(scratch, "rejected")], {
      cwd: REPO,
      env: { ...process.env, WITCHY: BIN, WITCHY_BROWSER_WASM: missingCompiler },
      stdio: "pipe",
    });
  } catch {
    rejectedMissingCompiler = true;
  }
  ok(rejectedMissingCompiler, "a complete bundle rejects an explicitly missing browser compiler");

  const fakeCargo = join(scratch, "cargo");
  const fakeTarget = join(scratch, "target");
  const completeDist = join(scratch, "complete");
  writeFileSync(fakeCargo, `#!/bin/sh
set -eu
out="$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/witchy.wasm"
mkdir -p "$(dirname "$out")"
printf 'fresh browser compiler' >"$out"
`);
  chmodSync(fakeCargo, 0o755);
  execFileSync("bash", [SCRIPT, completeDist], {
    cwd: REPO,
    env: {
      ...process.env,
      PATH: `${dirname(process.execPath)}:/usr/bin:/bin`,
      WITCHY: BIN,
      WITCHY_BROWSER_WASM: "",
      CARGO: fakeCargo,
      CARGO_TARGET_DIR: fakeTarget,
      WITCHY_SKIP_WASM_OPT: "1",
      WITCHY_BOOK_BASE: "/witchy/",
    },
    stdio: "pipe",
  });
  ok(
    readFileSync(join(completeDist, "witchy.wasm"), "utf8") === "fresh browser compiler",
    "a complete bundle builds its browser compiler from the current checkout",
  );

  const inertDist = join(scratch, "inert");
  execFileSync("bash", [SCRIPT, "--allow-missing-compiler", inertDist], {
    cwd: REPO,
    env: { ...process.env, WITCHY: BIN, WITCHY_BROWSER_WASM: missingCompiler },
    stdio: "pipe",
  });
  ok(!existsSync(join(inertDist, "witchy.wasm")), "the explicit non-runnable bundle contains no browser compiler");
  ok(!existsSync(join(inertDist, "docs-static-boot.js")), "the non-runnable bundle contains no runnable host");
  ok(
    filesUnder(inertDist)
      .filter((path) => path.endsWith(".html"))
      .every((path) => !readFileSync(path, "utf8").includes("data-witchy-runnables")),
    "the non-runnable bundle leaves checked code fences inert",
  );

  for (const path of [
    "index.html",
    "_headers",
    "witchy-web-manifest.json",
    "witchy-islands-manifest.json",
    "witchy-island-artifacts.json",
    "witchy-build-report.json",
    "witchy-sbom.cdx.json",
    "docs-static-boot.js",
    "witchy-runnable.js",
    "witchy-host.js",
    "witchy-cell-sandbox.js",
    "witchy-cell-frame.js",
    "witchy-runtime/witchy-runtime.mjs",
    "examples.json",
    "witchy.wasm",
  ]) {
    ok(existsSync(join(completeDist, path)), `the complete bundle contains ${path}`);
  }
  for (const retired of [
    "docs.wasm",
    "counter.wasm",
    "docs-boot.js",
    "glamour-dom.mjs",
    "rfc0103-browser-probe.html",
    "rfc0103-browser-probe.js",
    "content/SUMMARY.md",
  ]) {
    ok(!existsSync(join(completeDist, retired)), `the complete bundle omits retired ${retired}`);
  }

  const manifest = json(join(completeDist, "witchy-web-manifest.json"));
  ok(manifest.delivery === "static", "the book uses native static delivery");
  ok(manifest.routes.length === 56, "the book publishes all 56 canonical routes");
  ok(manifest.contentInputs.length === 56, "the build authenticates all 56 Markdown inputs");
  ok(
    manifest.runtime?.javascript === true && manifest.runtime?.wasm === true,
    "the manifest reports runtime code because the book contains an interactive region",
  );
  ok(
    manifest.hostFacilities?.schema === "witchy.book.runnable-host.v1"
      && manifest.hostFacilities?.bundleIdentity === manifest.bundleIdentity,
    "the runnable-cell host has an explicit authenticated bundle identity",
  );
  ok(
    manifest.hostFacilities?.isolation === "opaque-frame",
    "runnable programs execute behind the opaque-frame boundary",
  );
  ok(
    manifest.hostFacilities?.basePath === "/witchy/"
      && manifest.hostFacilities?.loader === "/witchy/docs-static-boot.js"
      && manifest.hostFacilities?.compiler === "/witchy/witchy.wasm",
    "the packaged host records the GitHub Pages project base",
  );

  const islands = json(join(completeDist, "witchy-islands-manifest.json"));
  const counter = islands.islands.find((island) => island.name === "counter");
  ok(
    counter?.activation === "interaction" && counter.mode === "resume",
    "the counter is a compiler-lowered interaction-activated resumable island",
  );

  const runnableRoutes = [];
  const islandRoutes = [];
  let runnableCells = 0;
  for (const route of manifest.routes) {
    const html = readFileSync(join(completeDist, route.file), "utf8");
    const runnable = html.includes('data-witchy-runnable="1"');
    const runner = html.includes("data-witchy-runnables");
    const island = html.includes("data-glamour-island");
    ok(runnable === runner, `${route.path} loads the runner exactly when it has runnable fences`);
    if (runnable) {
      runnableRoutes.push(route.path);
      runnableCells += html.match(/data-witchy-runnable="1"/g)?.length || 0;
    }
    if (island) {
      islandRoutes.push(route.path);
      const routeManifestFile = manifest.islands?.routes?.[route.path];
      ok(
        typeof routeManifestFile === "string"
          && html.includes(`data-witchy-islands-manifest="${routeManifestFile}"`),
        `${route.path} selects its compiler-published route manifest`,
      );
      const routeManifest = json(join(completeDist, routeManifestFile));
      const pageIslandCount = html.match(/data-glamour-island="/g)?.length || 0;
      ok(
        routeManifest.islands.length === pageIslandCount
          && routeManifest.islands.every((record) => html.includes(`data-glamour-island="${record.id}"`)),
        `${route.path} has an exact DOM-to-manifest island join`,
      );
    } else {
      ok(manifest.islands?.routes?.[route.path] === undefined, `${route.path} has no route manifest`);
    }
  }
  const runners = islands.islands.filter((island) => island.name === "runnable-fence");
  ok(
    runners.length === runnableCells
      && runners.every((island) => island.activation === "load" && island.mode === "resume"),
    "every editable fence is a load-activated compiler-resumable island",
  );
  ok(
    JSON.stringify(runnableRoutes) === JSON.stringify(manifest.hostFacilities.routes),
    "the recorded runnable route graph matches emitted HTML",
  );
  ok(
    runnableRoutes.every((route) => islandRoutes.includes(route))
      && islandRoutes.includes("/p/appendix-recipes"),
    "every runnable route carries fence islands and the recipe route carries its counter",
  );
  const recipe = readFileSync(join(completeDist, "p/appendix-recipes/index.html"), "utf8");
  ok(recipe.includes("counter-demo") && recipe.includes("data-witchy-islands"), "the recipe page ships server-rendered counter HTML and its island loader");
  ok(
    manifest.routes.every((route) => {
      const html = readFileSync(join(completeDist, route.file), "utf8");
      return !/<[^>]+\b(?:href|src|action)="\/(?!witchy\/|\/)/.test(html);
    }),
    "every root-relative HTML URL is rebased beneath the GitHub Pages project path",
  );

  const examples = json(join(REPO, "book/examples.json"));
  const runnableSources = new Set(examples
    .filter((entry) => entry.browser_runnable && entry.file.startsWith("book/src/"))
    .map((entry) => entry.file.slice("book/src/".length)));
  const nonRunnableRoutes = manifest.routes.filter((route) => {
    const source = route.path === "/" ? "introduction.md" : `${route.path.slice(3)}.md`;
    return !runnableSources.has(source);
  });
  ok(nonRunnableRoutes.length > 0, "the book has routes classified as non-runnable");
  ok(
    nonRunnableRoutes.every((route) => {
      const html = readFileSync(join(completeDist, route.file), "utf8");
      return !html.includes("<script")
        && !html.includes('data-witchy-runnable="1"')
        && !html.includes("data-glamour-island");
    }),
    "non-runnable routes ship no script, Wasm loader, runnable marker, or island",
  );

  const headers = readFileSync(join(completeDist, "_headers"), "utf8");
  ok(
    !headers.includes("Content-Security-Policy:")
      && headers.includes("Referrer-Policy: no-referrer")
      && headers.includes("X-Content-Type-Options: nosniff")
      && headers.includes("Content-Type: application/wasm"),
    "the bundle keeps security and Wasm headers without a parent CSP that would disable opaque srcdoc frames",
  );
  ok(
    manifest.hostFacilities?.parentContentSecurityPolicy === "omitted"
      && manifest.hostFacilities?.sandboxContentSecurityPolicy === "capability-derived",
    "the manifest records the relaxed parent policy and capability-derived sandbox policy",
  );

  const report = json(join(completeDist, "witchy-build-report.json"));
  const recorded = new Map(report.artifacts.map((artifact) => [artifact.path, artifact]));
  const emitted = filesUnder(completeDist)
    .filter((path) => path !== join(completeDist, "witchy-build-report.json"));
  ok(recorded.size === emitted.length, "the build report records every emitted bundle artifact");
  ok(emitted.every((path) => {
    const bytes = readFileSync(path);
    const name = relative(completeDist, path).split(sep).join("/");
    const artifact = recorded.get(name);
    return artifact?.bytes === bytes.length && artifact?.sha256 === sha256(bytes);
  }), "every recorded bundle artifact has the emitted byte length and SHA-256 digest");
  ok(
    report.bundleIdentity === manifest.bundleIdentity
      && report.hostFacilities?.routes === runnableRoutes.length,
    "the report and manifest agree on the packaged book identity and host scope",
  );
} finally {
  rmSync(scratch, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-DOCS-BUNDLE FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-DOCS-BUNDLE OK");

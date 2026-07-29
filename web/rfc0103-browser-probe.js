import { createSandboxedProgramRunner, probeSandboxFetch } from "./witchy-cell-sandbox.js";
import { DOCS_SANDBOX_RUN_OPTIONS } from "./docs-run-options.js";
import { fetchWasm } from "./wasm-fetch.js";

const result = document.getElementById("result");
const params = new URLSearchParams(location.search);
const allowedOrigin = params.get("allowed");
const blockedOrigin = params.get("blocked");

function extractWitchyBlocks(markdown) {
  const blocks = [];
  let current = null;
  for (const line of markdown.split("\n")) {
    if (
      current === null
      && /^```witchy(?:-runnable|-static)?$/.test(line.trimEnd())
    ) {
      current = [];
    } else if (current !== null && line.trimEnd() === "```") {
      blocks.push(current.join("\n"));
      current = null;
    } else if (current !== null) {
      current.push(line);
    }
  }
  return blocks;
}

async function loadCompiler() {
  const bytes = await fetchWasm("./witchy.wasm");
  const { module, instance } = await WebAssembly.instantiate(bytes, {});
  return { bytes, module, exports: instance.exports };
}

async function run() {
  if (!allowedOrigin || !blockedOrigin) throw new Error("missing allowed/blocked probe origins");
  const fetchCaps = { fetch: { origins: [allowedOrigin] } };
  const allowed = await probeSandboxFetch(`${allowedOrigin}/probe-allowed`, fetchCaps);
  if (!allowed.ok) throw new Error(`granted origin failed: ${allowed.text || allowed.status}`);
  const blocked = await probeSandboxFetch(`${blockedOrigin}/probe-blocked`, fetchCaps);
  if (blocked.ok) throw new Error("ungranted origin was reachable");

  const manifest = await fetch("./examples.json").then((response) => response.json());
  const pages = new Map();
  const runner = createSandboxedProgramRunner({ document, loadCompiler, timeoutMs: 60_000 });
  const [flagshipSource, flagshipPlan] = await Promise.all([
    fetch("./fixture-showcase/fixture_showcase.witchy").then((response) => {
      if (!response.ok) throw new Error(`failed to load flagship source: HTTP ${response.status}`);
      return response.text();
    }),
    fetch("./fixture-showcase/release.fixture.json").then((response) => {
      if (!response.ok) throw new Error(`failed to load flagship plan: HTTP ${response.status}`);
      return response.text();
    }),
  ]);
  const flagship = await runner(flagshipSource, { fixturePlan: flagshipPlan });
  if (!flagship.ok || flagship.text !== "release api at 1700000000000ms in staging") {
    throw new Error(
      `flagship fixture expected deterministic release output, got ${JSON.stringify(flagship)}`,
    );
  }
  const flagshipFamilies = new Set(
    (flagship.transcript?.events || []).map((event) => event.family),
  );
  for (const family of ["console", "clock", "env", "argv"]) {
    if (!flagshipFamilies.has(family)) {
      throw new Error(`flagship fixture transcript omitted ${family}`);
    }
  }

  let complete = 0;
  let exact = 0;
  for (const entry of manifest) {
    if (!entry.file.startsWith("book/src/") || entry.expect_error) continue;
    if (!pages.has(entry.file)) {
      const name = entry.file.slice("book/src/".length);
      const markdown = await fetch(`./content/${name}`).then((response) => {
        if (!response.ok) throw new Error(`failed to load ${entry.file}: HTTP ${response.status}`);
        return response.text();
      });
      pages.set(entry.file, extractWitchyBlocks(markdown));
    }
    const source = pages.get(entry.file)[entry.block - 1];
    if (source === undefined) throw new Error(`${entry.file} #${entry.block}: block index drift`);
    if (!/^\s*(?:pub\s+)?fn\s+main\s*\(/m.test(source)) continue;
    complete++;
    if (!entry.browser_runnable) {
      throw new Error(`${entry.file} #${entry.block}: complete example is not browser-runnable`);
    }
    const execution = await runner(source, DOCS_SANDBOX_RUN_OPTIONS);
    if (!execution.ok) {
      throw new Error(`${entry.file} #${entry.block}: ${execution.text}`);
    }
    if (entry.runnable) {
      const exactExecution = await runner(source, {});
      const expected = (entry.output || []).join("\n");
      if (!exactExecution.ok || exactExecution.text !== expected) {
        throw new Error(
          `${entry.file} #${entry.block}: expected ${JSON.stringify(expected)}, `
          + `got ${JSON.stringify(exactExecution.text)}`,
        );
      }
      exact++;
    }
  }
  if (complete === 0 || exact === 0) throw new Error("browser example probe was vacuous");
  result.dataset.state = "pass";
  result.dataset.flagship = "pass";
  result.dataset.complete = String(complete);
  result.dataset.exact = String(exact);
  result.textContent =
    `PASS: flagship fixture passed; CSP blocked the ungranted origin; ${complete} complete examples ran `
    + `in opaque frames (${exact} exact manifest outputs)`;
}

run().catch((error) => {
  result.dataset.state = "fail";
  result.textContent = `FAIL: ${String((error && error.stack) || error)}`;
});

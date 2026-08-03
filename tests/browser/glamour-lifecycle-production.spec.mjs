import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { extname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

const here = fileURLToPath(new URL(".", import.meta.url));
const repo = resolve(here, "../..");
const binary = process.env.WITCHY_BIN || resolve(repo, "target/debug/witchy");
const project = resolve(repo, "projects/glamour/examples/browser_lifecycle");
const output = mkdtempSync(join(tmpdir(), "witchy-glamour-lifecycle-"));

execFileSync(binary, ["build", "--web", "--out", output, project], {
  cwd: repo,
  encoding: "utf8",
});

test.afterAll(() => rmSync(output, { recursive: true, force: true }));

const contentTypes = new Map([
  [".html", "text/html; charset=utf-8"],
  [".json", "application/json; charset=utf-8"],
  [".mjs", "text/javascript; charset=utf-8"],
  [".wasm", "application/wasm"],
]);
const manifest = JSON.parse(readFileSync(join(output, "witchy-islands-manifest.json"), "utf8"));
const artifacts = JSON.parse(readFileSync(join(output, "witchy-island-artifacts.json"), "utf8"));
const byName = new Map(manifest.islands.map((island) => [island.name, island]));
const byArtifact = new Map(artifacts.artifacts.map((artifact) => [artifact.artifact, artifact]));
const artifactPath = (name) => byArtifact.get(byName.get(name).artifact).url;
const defaultWasm = artifactPath("default-visible");
const prefetchedWasm = artifactPath("prefetch-visible");
const recoveryWasm = artifactPath("native-recovery");

expect(byName.get("default-visible")).toMatchObject({ activation: "visible", prefetch: "none" });
expect(byName.get("prefetch-visible")).toMatchObject({ activation: "interaction", prefetch: "visible" });
expect(byName.get("native-recovery")).toMatchObject({ activation: "interaction", prefetch: "none" });

async function publishedPage(browser, { failRecovery = false } = {}) {
  const page = await browser.newPage();
  const requests = [];
  const errors = [];
  await page.addInitScript(() => {
    const compile = WebAssembly.compile.bind(WebAssembly);
    globalThis.__witchyCompileCount = 0;
    WebAssembly.compile = async (...args) => {
      globalThis.__witchyCompileCount += 1;
      return compile(...args);
    };
  });
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("request", (request) => requests.push({
    method: request.method(),
    pathname: new URL(request.url()).pathname,
  }));
  await page.route("http://witchy.test/**", async (route) => {
    const request = route.request();
    const pathname = decodeURIComponent(new URL(request.url()).pathname);
    if (pathname === "/next" || pathname === "/save") {
      await route.fulfill({
        status: 200,
        contentType: "text/html; charset=utf-8",
        body: `<!doctype html><title>${pathname}</title>`,
      });
      return;
    }
    if (failRecovery && pathname === recoveryWasm) {
      await route.fulfill({ status: 503, body: "intentional recovery failure" });
      return;
    }
    const relative = pathname === "/" ? "index.html" : pathname.slice(1);
    const file = resolve(output, relative);
    if (!file.startsWith(`${output}${sep}`)) {
      await route.fulfill({ status: 400, body: "invalid path" });
      return;
    }
    try {
      await route.fulfill({
        status: 200,
        contentType: contentTypes.get(extname(file)) || "application/octet-stream",
        body: readFileSync(file),
      });
    } catch {
      await route.fulfill({ status: 404, body: "not found" });
    }
  });
  return { page, requests, errors };
}

test("published lifecycle policies activate, prefetch bytes, and recover native defaults", async ({ browser }) => {
  const active = await publishedPage(browser);
  await active.page.goto("http://witchy.test/");
  await expect.poll(async () => ({
    compileCount: await active.page.evaluate(() => globalThis.__witchyCompileCount),
    wasm: [...new Set(active.requests.filter(({ pathname }) => pathname.endsWith(".wasm")).map(({ pathname }) => pathname))].sort(),
  }), { message: "visible activation compiles while visible prefetch retrieves bytes only" }).toEqual({
    compileCount: 1,
    wasm: [defaultWasm, prefetchedWasm].sort(),
  });
  await active.page.getByRole("button", { name: "Default 7" }).click();
  await expect(active.page.getByRole("button", { name: "Default 8" })).toBeVisible();
  await active.page.getByRole("button", { name: "Prefetched false" }).click();
  await expect(active.page.getByRole("button", { name: "Prefetched true" })).toBeVisible();
  expect(await active.page.evaluate(() => globalThis.__witchyCompileCount)).toBe(2);
  expect(active.requests.filter(({ pathname }) => pathname === prefetchedWasm)).toHaveLength(1);
  expect(active.errors).toEqual([]);
  await active.page.close();

  const navigate = await publishedPage(browser, { failRecovery: true });
  await navigate.page.goto("http://witchy.test/");
  await navigate.page.getByRole("link", { name: "Recover link 0" }).click();
  await expect(navigate.page).toHaveURL("http://witchy.test/next");
  expect(navigate.requests.filter(({ pathname }) => pathname === recoveryWasm)).toHaveLength(1);
  expect(navigate.requests.filter(({ method, pathname }) => method === "GET" && pathname === "/next")).toHaveLength(1);
  await navigate.page.close();

  const submit = await publishedPage(browser, { failRecovery: true });
  await submit.page.goto("http://witchy.test/");
  await submit.page.getByRole("button", { name: "Recover form" }).click();
  await expect(submit.page).toHaveURL("http://witchy.test/save");
  expect(submit.requests.filter(({ pathname }) => pathname === recoveryWasm)).toHaveLength(1);
  expect(submit.requests.filter(({ method, pathname }) => method === "POST" && pathname === "/save")).toHaveLength(1);
  await submit.page.close();
});

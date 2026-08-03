import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { extname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

const here = fileURLToPath(new URL(".", import.meta.url));
const repo = resolve(here, "../..");
const binary = process.env.WITCHY_BIN || resolve(repo, "target/debug/witchy");
const project = resolve(repo, "projects/glamour/examples/interactive");
const output = mkdtempSync(join(tmpdir(), "witchy-glamour-islands-"));

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

test("published resumable and fresh regions activate their authenticated Wasm", async ({ page }) => {
  const errors = [];
  const wasmRequests = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("request", (request) => {
    if (new URL(request.url()).pathname.endsWith(".wasm")) wasmRequests.push(request.url());
  });
  await page.route("http://witchy.test/**", async (route) => {
    const pathname = decodeURIComponent(new URL(route.request().url()).pathname);
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

  const manifests = Promise.all([
    page.waitForResponse((response) => response.url().endsWith("/witchy-islands-manifest.json")),
    page.waitForResponse((response) => response.url().endsWith("/witchy-island-artifacts.json")),
  ]);
  await page.goto("http://witchy.test/");
  await manifests;

  const resumed = page.getByRole("button", { name: "7" });
  const resumedBefore = await resumed.elementHandle();
  await expect(resumed).toBeVisible();
  expect(wasmRequests).toHaveLength(0);
  await resumed.click();
  await expect.poll(async () => ({ buttons: await page.locator("button").allTextContents(), errors, wasmRequests: wasmRequests.length }), {
    message: "resumable activation should patch the adopted button without browser errors",
  }).toEqual({ buttons: ["8", "Load editor"], errors: [], wasmRequests: 1 });
  const resumedAfter = await page.getByRole("button", { name: "8" }).elementHandle();
  expect(await page.evaluate(([before, after]) => before === after, [resumedBefore, resumedAfter])).toBe(true);
  expect(wasmRequests).toHaveLength(1);

  const freshFallback = page.getByRole("button", { name: "Load editor" });
  await freshFallback.click();
  await expect.poll(async () => ({ buttons: await page.locator("button").allTextContents(), errors, wasmRequests: wasmRequests.length }), {
    message: "fresh activation should replace the fallback without replaying its trigger",
  }).toEqual({ buttons: ["8", "42"], errors: [], wasmRequests: 2 });
  const fresh = page.getByRole("button", { name: "42" });
  await fresh.click();
  await expect(page.getByRole("button", { name: "43" })).toBeVisible();
  expect(errors).toEqual([]);
});

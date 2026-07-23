#!/usr/bin/env node
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { readFileSync, statSync } from "node:fs";
import { extname, join, resolve, sep } from "node:path";

const root = resolve(process.argv[2] || "dist");
const required = [
  "rfc0103-browser-probe.html",
  "rfc0103-browser-probe.js",
  "witchy-cell-sandbox.js",
  "witchy.wasm",
  "examples.json",
];
for (const file of required) statSync(join(root, file));

const mime = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".json": "application/json",
  ".md": "text/markdown; charset=utf-8",
  ".wasm": "application/wasm",
};
let allowedHits = 0;
let blockedHits = 0;

function listen(server) {
  return new Promise((resolveListen, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolveListen(server.address().port));
  });
}
const staticServer = createServer((request, response) => {
  if (request.url.startsWith("/probe-allowed")) {
    allowedHits++;
    response.writeHead(200, {
      "Access-Control-Allow-Origin": "*",
      "Content-Type": "text/plain",
      "Cache-Control": "no-store",
    });
    response.end("allowed");
    return;
  }
  const pathname = new URL(request.url, "http://localhost").pathname;
  const relative = pathname === "/" ? "rfc0103-browser-probe.html" : pathname.slice(1);
  const path = resolve(root, relative);
  if (path !== root && !path.startsWith(root + sep)) {
    response.writeHead(403).end();
    return;
  }
  try {
    const bytes = readFileSync(path);
    response.writeHead(200, {
      "Content-Type": mime[extname(path)] || "application/octet-stream",
      "Cache-Control": "no-store",
    });
    response.end(bytes);
  } catch {
    response.writeHead(404).end();
  }
});
const blockedServer = createServer((_request, response) => {
  blockedHits++;
  response.writeHead(200, {
    "Access-Control-Allow-Origin": "*",
    "Content-Type": "text/plain",
    "Cache-Control": "no-store",
  });
  response.end("this request must never arrive");
});

async function webdriver(port, method, path, body) {
  const response = await fetch(`http://127.0.0.1:${port}${path}`, {
    method,
    headers: { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const value = await response.json();
  if (!response.ok || (value.value && value.value.error)) {
    throw new Error(`WebDriver ${method} ${path}: ${JSON.stringify(value)}`);
  }
  return value.value;
}

async function waitForDriver(port, child) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (child.exitCode !== null) throw new Error(`safaridriver exited ${child.exitCode}`);
    try {
      await fetch(`http://127.0.0.1:${port}/status`);
      return;
    } catch {
      await new Promise((resolveWait) => setTimeout(resolveWait, 100));
    }
  }
  throw new Error("safaridriver did not become ready");
}

async function freePort() {
  const server = createServer();
  const port = await listen(server);
  await new Promise((resolveClose) => server.close(resolveClose));
  return port;
}

let driver;
let session;
let driverPort;
try {
  const staticPort = await listen(staticServer);
  const blockedPort = await listen(blockedServer);
  driverPort = await freePort();
  driver = spawn("safaridriver", ["--port", String(driverPort)], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  let driverError = "";
  driver.stderr.on("data", (chunk) => { driverError += chunk; });
  await waitForDriver(driverPort, driver);
  const created = await webdriver(driverPort, "POST", "/session", {
    capabilities: { alwaysMatch: { browserName: "safari" } },
  });
  session = created.sessionId;
  const page =
    `http://127.0.0.1:${staticPort}/rfc0103-browser-probe.html`
    + `?allowed=${encodeURIComponent(`http://127.0.0.1:${staticPort}`)}`
    + `&blocked=${encodeURIComponent(`http://127.0.0.1:${blockedPort}`)}`;
  await webdriver(driverPort, "POST", `/session/${session}/url`, { url: page });

  let state;
  const deadline = Date.now() + 240_000;
  while (Date.now() < deadline) {
    state = await webdriver(driverPort, "POST", `/session/${session}/execute/sync`, {
      script: `const e = document.getElementById("result"); return {
        state: e && e.dataset.state,
        text: e && e.textContent,
        complete: e && e.dataset.complete,
        exact: e && e.dataset.exact,
      };`,
      args: [],
    });
    if (state && state.state !== "running") break;
    await new Promise((resolveWait) => setTimeout(resolveWait, 250));
  }
  if (!state || state.state !== "pass") {
    throw new Error(`browser probe failed: ${JSON.stringify(state)} ${driverError}`);
  }
  if (allowedHits === 0) throw new Error("positive-control origin received no request");
  if (blockedHits !== 0) {
    throw new Error(`CSP failed: ungranted origin received ${blockedHits} request(s)`);
  }
  console.log(
    `RFC-0103 BROWSER PASS: blocked origin received 0 requests; `
    + `${state.complete} complete examples ran in opaque frames `
    + `(${state.exact} exact manifest outputs)`,
  );
} finally {
  if (session && driver && driver.exitCode === null) {
    try {
      await webdriver(driverPort, "DELETE", `/session/${session}`);
    } catch {
      // The process cleanup below is authoritative.
    }
  }
  if (driver && driver.exitCode === null) driver.kill("SIGTERM");
  await Promise.all([
    new Promise((resolveClose) => staticServer.close(resolveClose)),
    new Promise((resolveClose) => blockedServer.close(resolveClose)),
  ]);
}

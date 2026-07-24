#!/usr/bin/env node
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import { mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { extname, join, resolve, sep } from "node:path";

const root = resolve(process.argv[2] || "dist");
const browser = process.argv[3] || "safari";
if (!["safari", "chrome"].includes(browser)) {
  throw new Error(`unsupported browser ${JSON.stringify(browser)}; expected safari or chrome`);
}
const required = [
  "rfc0103-browser-probe.html",
  "rfc0103-browser-probe.js",
  "witchy-cell-sandbox.js",
  "witchy-cell-frame.js",
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
const staticRequests = [];

function listen(server) {
  return new Promise((resolveListen, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolveListen(server.address().port));
  });
}
const staticServer = createServer((request, response) => {
  staticRequests.push(request.url);
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

async function waitForChrome(port, child) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (child.exitCode !== null) throw new Error(`Chrome exited ${child.exitCode}`);
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/list`);
      if (response.ok) {
        const targets = await response.json();
        const page = targets.find((target) => target.type === "page");
        if (page) return page;
      }
    } catch {
      // Chrome has not opened its debugging socket yet.
    }
    await new Promise((resolveWait) => setTimeout(resolveWait, 100));
  }
  throw new Error("Chrome DevTools did not become ready");
}

async function connectCdp(url, onEvent) {
  const socket = new WebSocket(url);
  await new Promise((resolveOpen, reject) => {
    socket.addEventListener("open", resolveOpen, { once: true });
    socket.addEventListener("error", reject, { once: true });
  });
  let nextId = 1;
  const pending = new Map();
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    if (!message.id) {
      onEvent(message);
      return;
    }
    if (!pending.has(message.id)) return;
    const { resolveMessage, reject } = pending.get(message.id);
    pending.delete(message.id);
    if (message.error) reject(new Error(`CDP: ${JSON.stringify(message.error)}`));
    else resolveMessage(message.result);
  });
  function send(method, params = {}) {
    const id = nextId++;
    return new Promise((resolveMessage, reject) => {
      pending.set(id, { resolveMessage, reject });
      socket.send(JSON.stringify({ id, method, params }));
    });
  }
  return {
    async enableDiagnostics() {
      await Promise.all([send("Log.enable"), send("Runtime.enable")]);
    },
    async evaluate(expression) {
      const response = await send("Runtime.evaluate", {
        expression,
        returnByValue: true,
      });
      if (response.exceptionDetails) {
        throw new Error(`browser evaluation failed: ${JSON.stringify(response.exceptionDetails)}`);
      }
      return response.result.value;
    },
    close() {
      socket.close();
    },
  };
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
let cdp;
let chromeProfile;
try {
  const staticPort = await listen(staticServer);
  const blockedPort = await listen(blockedServer);
  driverPort = await freePort();
  let driverError = "";
  const page =
    `http://127.0.0.1:${staticPort}/rfc0103-browser-probe.html`
    + `?allowed=${encodeURIComponent(`http://127.0.0.1:${staticPort}`)}`
    + `&blocked=${encodeURIComponent(`http://127.0.0.1:${blockedPort}`)}`;
  let evaluate;
  if (browser === "safari") {
    driver = spawn("safaridriver", ["--port", String(driverPort)], {
      stdio: ["ignore", "pipe", "pipe"],
    });
    driver.stderr.on("data", (chunk) => { driverError += chunk; });
    await waitForDriver(driverPort, driver);
    const created = await webdriver(driverPort, "POST", "/session", {
      capabilities: { alwaysMatch: { browserName: "safari" } },
    });
    session = created.sessionId;
    await webdriver(driverPort, "POST", `/session/${session}/url`, { url: page });
    evaluate = (script) => webdriver(
      driverPort,
      "POST",
      `/session/${session}/execute/sync`,
      { script, args: [] },
    );
  } else {
    chromeProfile = mkdtempSync(join(tmpdir(), "witchy-rfc0103-chrome-"));
    driver = spawn(
      process.env.CHROME_BIN
        || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
      [
        "--headless=new",
        `--remote-debugging-port=${driverPort}`,
        `--user-data-dir=${chromeProfile}`,
        "--no-first-run",
        "--no-default-browser-check",
        page,
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
    driver.stderr.on("data", (chunk) => { driverError += chunk; });
    const target = await waitForChrome(driverPort, driver);
    cdp = await connectCdp(target.webSocketDebuggerUrl, (event) => {
      if (event.method === "Log.entryAdded"
          || event.method === "Runtime.exceptionThrown"
          || event.method === "Runtime.consoleAPICalled") {
        driverError += `\nCDP ${event.method}: ${JSON.stringify(event.params)}`;
      }
    });
    await cdp.enableDiagnostics();
    evaluate = (script) => cdp.evaluate(script);
  }

  let state;
  const deadline = Date.now() + 240_000;
  while (Date.now() < deadline) {
    state = await evaluate(`(() => {
      const e = document.getElementById("result"); return {
        state: e && e.dataset.state,
        text: e && e.textContent,
        flagship: e && e.dataset.flagship,
        complete: e && e.dataset.complete,
        exact: e && e.dataset.exact,
      };
    })()`);
    if (state && state.state !== "running") break;
    await new Promise((resolveWait) => setTimeout(resolveWait, 250));
  }
  if (!state || state.state !== "pass") {
    throw new Error(
      `browser probe failed: ${JSON.stringify(state)}; `
      + `static requests: ${JSON.stringify(staticRequests)} ${driverError}`,
    );
  }
  if (state.flagship !== "pass") {
    throw new Error(`browser probe omitted the flagship fixture result: ${JSON.stringify(state)}`);
  }
  if (allowedHits === 0) throw new Error("positive-control origin received no request");
  if (blockedHits !== 0) {
    throw new Error(`CSP failed: ungranted origin received ${blockedHits} request(s)`);
  }
  console.log(
    `RFC-0103 BROWSER PASS: flagship fixture passed; blocked origin received 0 requests; `
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
  if (cdp) cdp.close();
  if (driver && driver.exitCode === null) {
    driver.kill("SIGTERM");
    await Promise.race([
      once(driver, "exit"),
      new Promise((resolveWait) => setTimeout(resolveWait, 2_000)),
    ]);
  }
  if (chromeProfile) rmSync(chromeProfile, { force: true, recursive: true });
  await Promise.all([
    new Promise((resolveClose) => staticServer.close(resolveClose)),
    new Promise((resolveClose) => blockedServer.close(resolveClose)),
  ]);
}

// Drive the local HTTPS witness in an isolated Chromium profile.
// The Python witness independently verifies the fixture CA over HTTPS.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

const [chrome, origin, leafPin, expectedRows = "3"] = process.argv.slice(2);
assert.equal(new URL(origin).hostname, "localhost");
assert.match(leafPin, /^[A-Za-z0-9+/]{43}=$/);
const profile = await mkdtemp(join(tmpdir(), "coven-chromium-"));
const child = spawn(chrome, [
    "--headless", "--no-first-run", "--no-default-browser-check", "--disable-background-networking",
    "--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0",
    `--user-data-dir=${profile}`, `--ignore-certificate-errors-spki-list=${leafPin}`, "about:blank",
], { stdio: "ignore" });
let socket;
const exited = new Promise((resolve, reject) => {
    child.once("exit", resolve);
    child.once("error", reject);
});
exited.catch(() => {});

try {
    let port;
    for (let attempt = 0; attempt < 150; attempt++) {
        if (child.exitCode !== null) throw new Error("Chromium exited before starting DevTools");
        try {
            port = (await readFile(join(profile, "DevToolsActivePort"), "utf8")).split("\n")[0];
            if (port) break;
        } catch (error) {
            if (error.code !== "ENOENT") throw error;
        }
        await delay(50);
    }
    assert.match(port ?? "", /^[0-9]+$/);
    const pages = await (await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(5000) })).json();
    const target = pages.find(page => page.type === "page");
    assert.ok(target);
    socket = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("DevTools connection timed out")), 5000);
        socket.addEventListener("open", () => { clearTimeout(timer); resolve(); }, { once: true });
        socket.addEventListener("error", () => { clearTimeout(timer); reject(new Error("DevTools connection failed")); }, { once: true });
    });

    let serial = 0;
    function command(method, params = {}, sessionId) {
        const id = ++serial;
        return new Promise((resolve, reject) => {
            const timer = setTimeout(() => {
                socket.removeEventListener("message", receive);
                reject(new Error(`DevTools command timed out: ${method}`));
            }, 5000);
            function receive({ data }) {
                const reply = JSON.parse(data);
                if (reply.id !== id) return;
                clearTimeout(timer);
                socket.removeEventListener("message", receive);
                if (reply.error) reject(new Error(reply.error.message));
                else resolve(reply.result);
            }
            socket.addEventListener("message", receive);
            socket.send(JSON.stringify({ id, method, params, sessionId }));
        });
    }
    async function evaluate(expression, { contextId, sessionId } = {}) {
        const reply = await command("Runtime.evaluate", { expression, contextId, awaitPromise: true, returnByValue: true }, sessionId);
        assert.ok(!reply.exceptionDetails, JSON.stringify(reply.exceptionDetails));
        return reply.result.value;
    }

    const diagnostics = [];
    const frames = new Map();
    socket.addEventListener("message", ({ data }) => {
        const event = JSON.parse(data);
        if (event.method === "Target.attachedToTarget" && event.params.targetInfo.type === "iframe") {
            const sessionId = event.params.sessionId;
            frames.set(sessionId, { sessionId });
            command("Runtime.enable", {}, sessionId).catch(error => diagnostics.push(error.message));
        }
        if (event.method === "Target.detachedFromTarget")
            frames.delete(event.params.sessionId);
        if (event.method === "Runtime.executionContextCreated" && event.params.context.auxData?.isDefault) {
            const frame = frames.get(event.sessionId);
            if (frame) frame.contextId = event.params.context.id;
        }
        if (event.method === "Log.entryAdded") diagnostics.push(event.params.entry.text);
        if (event.method === "Network.loadingFailed") diagnostics.push(event.params.errorText);
    });
    await command("Target.setAutoAttach", { autoAttach: true, waitForDebuggerOnStart: false, flatten: true });
    await command("Log.enable");
    await command("Network.enable");
    const navigation = await command("Page.navigate", { url: origin });
    assert.ok(!navigation.errorText, navigation.errorText);
    let ready = false;
    for (let attempt = 0; attempt < 100; attempt++) {
        ready = await evaluate("document.querySelector('#catalog-filter')?.disabled === false");
        if (ready) break;
        await delay(50);
    }
    if (!ready) {
        const state = await evaluate("({ title: document.title, secure: isSecureContext, status: document.querySelector('#filter-status')?.textContent })");
        throw new Error(`Glamour's sandbox did not enable the catalog filter: ${JSON.stringify({ state, diagnostics })}`);
    }
    assert.equal(await evaluate("document.querySelectorAll('#catalog tbody > tr').length"), Number(expectedRows));
    assert.deepEqual(await evaluate(`(() => {
        const frame = document.querySelector('iframe[src="/glamour-frame.html"]');
        if (!frame) return null;
        return { sandbox: frame.sandbox.value, opaqueFromParent: frame.contentDocument === null };
    })()`), { sandbox: "allow-scripts", opaqueFromParent: true });
    let sandboxContext;
    for (let attempt = 0; attempt < 50; attempt++) {
        sandboxContext = Array.from(frames.values()).find(frame => frame.contextId);
        if (sandboxContext) break;
        await delay(25);
    }
    assert.ok(sandboxContext, `missing sandbox execution context: ${JSON.stringify(diagnostics)}`);
    assert.deepEqual(await evaluate(`(async () => {
        let parentBlocked = false;
        let storageBlocked = false;
        try { void parent.document; } catch (error) { parentBlocked = error.name === "SecurityError"; }
        try { void localStorage; } catch (error) { storageBlocked = error.name === "SecurityError"; }
        let networkBlocked = false;
        try { await fetch(location.href, { credentials: "omit" }); }
        catch (error) { networkBlocked = error instanceof TypeError; }
        return { origin, parentBlocked, storageBlocked, networkBlocked };
    })()`, sandboxContext), { origin: "null", parentBlocked: true, storageBlocked: true, networkBlocked: true });
    await evaluate(`(() => {
        const input = document.querySelector("#catalog-filter");
        input.value = "glamour";
        input.dispatchEvent(new Event("input"));
    })()`);
    let filtered = false;
    for (let attempt = 0; attempt < 100; attempt++) {
        filtered = await evaluate("document.querySelector('#filter-status')?.textContent === '1 matching rows.'");
        if (filtered) break;
        await delay(50);
    }
    assert.ok(filtered, "Glamour's Wasm filter did not yield one matching row");
    assert.equal(await evaluate("document.querySelector('#catalog tbody > tr:not([hidden]) a')?.textContent"), "glamour");
    assert.equal(await evaluate(`(() => {
        try { document.createElement("div").innerHTML = "<b>fixture</b>"; return false; }
        catch (error) { return error instanceof TypeError; }
    })()`), true);
    await evaluate(`document.querySelector('iframe[src="/glamour-frame.html"]').remove()`);
    for (let attempt = 0; frames.has(sandboxContext.sessionId) && attempt < 50; attempt++)
        await delay(25);
    assert.ok(!frames.has(sandboxContext.sessionId), "the browser did not detach the removed frame");
    await evaluate(`(() => {
        const input = document.querySelector("#catalog-filter");
        input.value = "";
        input.dispatchEvent(new Event("input"));
    })()`);
    let stopped = false;
    for (let attempt = 0; attempt < 60; attempt++) {
        stopped = await evaluate("document.querySelector('#catalog-filter').disabled");
        if (stopped) break;
        await delay(50);
    }
    assert.ok(stopped, "the filter deadline did not close the unresponsive channel");
    assert.equal(await evaluate("document.querySelector('#catalog tbody > tr:not([hidden]) a')?.textContent"), "glamour");
    await command("Network.setBlockedURLs", { urls: [new URL("/filter.wasm", origin).href] });
    await command("Page.reload", { ignoreCache: true });
    let fallback;
    for (let attempt = 0; attempt < 100; attempt++) {
        fallback = await evaluate(`(() => {
            const status = document.querySelector("#filter-status");
            if (!status || status.hidden || !status.textContent.startsWith("Filtering is unavailable"))
                return null;
            return {
                disabled: document.querySelector("#catalog-filter").disabled,
                rows: document.querySelectorAll("#catalog tbody > tr:not([hidden])").length,
                iframe: !!document.querySelector('iframe[src="/glamour-frame.html"]'),
            };
        })()`);
        if (fallback) break;
        await delay(50);
    }
    assert.deepEqual(fallback, { disabled: true, rows: Number(expectedRows), iframe: false });
    console.log("Chromium verified opaque frame isolation, Trusted Types, the frame deadline, and static fallback");
} finally {
    socket?.close();
    child.kill();
    await Promise.race([exited, delay(3000, undefined, { ref: false })]).catch(() => {});
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
    await rm(profile, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
}

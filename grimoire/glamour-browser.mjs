const encoder = new TextEncoder();
export function applyVisibility(rows, result) {
    if (!(result instanceof Uint8Array) || result.length !== rows.length + 1 ||
        result[0] !== 1 || result.some(value => value > 1) ||
        rows.some(row => !row.isConnected))
        throw new Error("invalid visibility update");
    rows.forEach((row, index) => { row.hidden = result[index + 1] === 0; });
}

function encodeEvent(value, names) {
    if (value.length > 64 || /[^\x20-\x7e]/.test(value))
        return null;
    const text = encoder.encode(value);
    const length = 2 + text.length + names.reduce((sum, name) => sum + 1 + name.length, 0);
    const event = new Uint8Array(length);
    event[0] = text.length;
    event.set(text, 1);
    let offset = text.length + 1;
    event[offset++] = names.length;
    for (const name of names) {
        event[offset++] = name.length;
        event.set(name, offset);
        offset += name.length;
    }
    return event;
}

async function pinnedBytes(path, expected, limit) {
    if (!/^[0-9a-f]{96}$/.test(expected)) throw new Error("invalid Glamour pin");
    const response = await fetch(path, {
        redirect: "error", credentials: "omit", signal: AbortSignal.timeout(1000)
    });
    if (!response.ok) throw new Error("missing Glamour asset");
    const reader = response.body.getReader();
    const bytes = new Uint8Array(limit);
    let used = 0;
    for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (value.length > bytes.length - used) {
            await reader.cancel();
            throw new Error("Glamour asset exceeds its limit");
        }
        bytes.set(value, used);
        used += value.length;
    }
    const wire = bytes.buffer.slice(0, used);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-384", wire));
    if (Array.from(digest, byte => byte.toString(16).padStart(2, "0")).join("") !== expected)
        throw new Error("Glamour asset differs from its pin");
    return wire;
}

function mountSandbox(open) {
    const frame = document.createElement("iframe");
    frame.setAttribute("sandbox", "allow-scripts");
    frame.setAttribute("referrerpolicy", "no-referrer");
    frame.setAttribute("title", "Glamour filter sandbox");
    frame.hidden = true;
    function ready(event) {
        if (event.source !== frame.contentWindow || event.origin !== "null" ||
            event.data !== "glamour-filter-ready-v1" || event.ports.length !== 1)
            return;
        window.removeEventListener("message", ready);
        open(event.ports[0]);
    }
    window.addEventListener("message", ready);
    frame.src = "/glamour-frame.html";
    document.body.append(frame);
    return () => {
        window.removeEventListener("message", ready);
        frame.remove();
    };
}

export async function mountFilter(query, rows, status) {
    if (!globalThis.trustedTypes || rows.length > 64)
        return;
    const names = rows.map(row => row.cells[0]?.textContent);
    if (names.some(name => typeof name !== "string" || !/^[\x20-\x7e]{1,64}$/.test(name)))
        return;
    const [module, source] = await Promise.all([
        pinnedBytes("/filter.wasm", document.querySelector('meta[name="glamour-island-sha384"]')?.content, 65536),
        pinnedBytes("/glamour-worker.mjs", document.querySelector('meta[name="glamour-worker-sha384"]')?.content, 16384),
    ]);
    let port;
    const encodedNames = names.map(name => encoder.encode(name));
    let pending;
    let active = 0;
    let serial = 0;
    function announce(text) {
        if (status) {
            status.hidden = false;
            status.textContent = text;
        }
    }
    function stop() {
        clearTimeout(timer);
        query.removeEventListener("input", update);
        query.disabled = true;
        port?.close();
        closeSandbox();
    }
    function fail() {
        stop();
        announce("Filtering stopped. The displayed rows are unchanged.");
    }
    let timer = setTimeout(fail, 1000);
    function send() {
        if (active || !pending) return;
        const event = pending;
        pending = undefined;
        active = ++serial;
        timer = setTimeout(fail, 1000);
        port.postMessage({ id: active, event, count: rows.length }, [event.buffer]);
    }
    function update() {
        pending = encodeEvent(query.value, encodedNames);
        if (!pending) announce("Use up to 64 ASCII characters.");
        send();
    }
    function receive({ data }) {
        clearTimeout(timer);
        if (data?.ready === true && query.disabled && !active) {
            query.addEventListener("input", update);
            query.disabled = false;
            return;
        }
        if (!active || data?.id !== active) {
            fail();
            return;
        }
        try {
            applyVisibility(rows, data.result);
        } catch {
            fail();
            return;
        }
        announce(`${rows.filter(row => !row.hidden).length} matching rows.`);
        active = 0;
        send();
    }
    const closeSandbox = mountSandbox(connection => {
        port = connection;
        port.onmessageerror = fail;
        port.onmessage = receive;
        port.postMessage({ source, module }, [source, module]);
    });
    window.addEventListener("pagehide", stop, { once: true });
}

const query = document.querySelector("input[data-glamour-filter]");
if (query) {
    const table = document.getElementById(query.dataset.glamourFilter);
    const status = document.getElementById(query.getAttribute("aria-describedby"));
    if (table) {
        mountFilter(query, Array.from(table.querySelectorAll("tbody > tr")), status).catch(() => {
            if (status) {
                status.textContent = "Filtering is unavailable. All rows are shown.";
                status.hidden = false;
            }
        });
    }
}

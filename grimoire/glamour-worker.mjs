// The guest can consume one bounded event and return one bounded visibility mask.
const io = ["print_u64", "print_u8", "print_word", "read_u8", "read_run"];
let compiled;

// Module.imports/exports cannot see private memory limits or start functions.
// Read only the section envelope and memory declarations. The engine validates code.
function checkMemory(buffer) {
    const bytes = new Uint8Array(buffer);
    const header = [0, 97, 115, 109, 1, 0, 0, 0];
    if (header.some((byte, index) => bytes[index] !== byte))
        throw new Error("invalid filter module header");
    let cursor = header.length;
    let limit = bytes.length;
    let pages = 0;
    function uint() {
        let value = 0;
        for (let shift = 0; shift <= 28 && cursor < limit; shift += 7) {
            const byte = bytes[cursor++];
            value += (byte & 127) * 2 ** shift;
            if (byte < 128 && value <= 0xffffffff) return value;
        }
        throw new Error("invalid filter section integer");
    }
    while (cursor < bytes.length) {
        limit = bytes.length;
        const section = bytes[cursor++];
        if (![0, 1, 2, 3, 5, 6, 7, 10].includes(section))
            throw new Error("invalid filter section");
        const size = uint();
        limit = cursor + size;
        if (limit > bytes.length)
            throw new Error("truncated filter section");
        if (section === 5) {
            if (pages) throw new Error("duplicate filter memory section");
            const count = uint();
            if (count < 1 || count > 2)
                throw new Error("invalid filter memory count");
            for (let index = 0; index < count; index++) {
                if (uint() !== 1)
                    throw new Error("filter memory must be private memory32 with a maximum");
                const initial = uint();
                if (initial < (index === 0 ? 2 : 1) || uint() !== initial)
                    throw new Error("filter memory must have a fixed size");
                pages += initial;
            }
            if (cursor !== limit || pages > 4)
                throw new Error("filter memory budget exceeded");
        }
        cursor = limit;
    }
    return pages > 0;
}

function compile(bytes) {
    if (!(bytes instanceof ArrayBuffer) || bytes.byteLength > 65536)
        throw new Error("invalid filter module size");
    const hasMemory = checkMemory(bytes);
    const module = new WebAssembly.Module(bytes);
    const imports = WebAssembly.Module.imports(module);
    const binding = imports[0]?.module;
    if (!["witchy.scalar.v1", "witchy.frame.v1"].includes(binding) ||
        hasMemory !== (binding === "witchy.frame.v1") ||
        imports.length !== io.length ||
        imports.some(({ module, name, kind }, i) =>
            module !== binding || name !== io[i] || kind !== "function"))
        throw new Error("invalid filter imports");
    const exports = WebAssembly.Module.exports(module);
    if (exports[0]?.name !== "run" || exports[0].kind !== "function" ||
        exports.length !== (binding === "witchy.frame.v1" ? 2 : 1) ||
        (exports.length === 2 && (exports[1].name !== "memory" || exports[1].kind !== "memory")))
        throw new Error("invalid filter exports");
    return { module, binding };
}

function filter(event, count) {
    if (!(event instanceof Uint8Array) || event.length > 4226 ||
        !Number.isInteger(count) || count < 0 || count > 64)
        throw new Error("invalid filter event");
    let read = 0;
    let written = 0;
    const result = new Uint8Array(count + 1);
    function unsupported() { throw new Error("invalid filter I/O"); }
    const instance = new WebAssembly.Instance(compiled.module, {
        [compiled.binding]: {
            read_u8(handle) {
                if (handle !== 3n || read > event.length) unsupported();
                return BigInt(event[read++] ?? 256);
            },
            read_run: unsupported,
            print_u64: unsupported,
            print_word: unsupported,
            print_u8(handle, value) {
                if (handle !== 1 || written >= result.length || (value !== 0n && value !== 1n))
                    unsupported();
                result[written++] = Number(value);
            },
        },
    });
    instance.exports.run();
    if (read !== event.length + 1 || written !== result.length || result[0] !== 1)
        throw new Error("incomplete filter result");
    return result;
}

self.onmessage = ({ data }) => {
    try {
        if (!compiled) {
            compiled = compile(data.module);
            self.postMessage({ ready: true });
            return;
        }
        const result = filter(data.event, data.count);
        self.postMessage({ id: data.id, result }, [result.buffer]);
    } catch {
        self.postMessage({ failed: true });
        self.close();
    }
};

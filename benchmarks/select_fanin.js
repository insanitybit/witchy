const { performance } = require('perf_hooks');

const perSource = 2048;

class Channel {
    constructor(capacity) {
        this.capacity = capacity;
        this.buffer = [];
        this.receivers = [];
        this.senders = [];
        this.closed = false;
    }

    async send(val) {
        if (this.receivers.length > 0) {
            const resolve = this.receivers.shift();
            resolve({ value: val, done: false });
            return;
        }
        if (this.buffer.length < this.capacity) {
            this.buffer.push(val);
            return;
        }
        await new Promise((resolve) => {
            this.senders.push(() => {
                this.buffer.push(val);
                resolve();
            });
        });
    }

    hasValue() {
        return this.buffer.length > 0;
    }

    async receive() {
        if (this.buffer.length > 0) {
            const val = this.buffer.shift();
            if (this.senders.length > 0) {
                const sendResolve = this.senders.shift();
                sendResolve();
            }
            return { value: val, done: false };
        }
        if (this.closed) {
            return { value: undefined, done: true };
        }
        return new Promise((resolve) => {
            this.receivers.push(resolve);
        });
    }
}

async function produce(ch, start, count) {
    for (let i = 0; i < count; i++) {
        await ch.send(start + i * 2);
    }
}

function expectedChecksum() {
    let checksum = 0;
    for (let i = 0; i < perSource; i++) {
        checksum = (checksum * 257 + i * 2 + 1) % 1000000007;
    }
    for (let i = 0; i < perSource; i++) {
        checksum = (checksum * 257 + i * 2 + 3) % 1000000007;
    }
    return checksum;
}

async function receivePriority(a, b) {
    if (a.hasValue()) {
        const item = await a.receive();
        return [0, item.value];
    }
    if (b.hasValue()) {
        const item = await b.receive();
        return [1, item.value];
    }
    const itemA = a.receive().then(r => [0, r.value]);
    const itemB = b.receive().then(r => [1, r.value]);
    return Promise.race([itemA, itemB]);
}

async function main() {
    const a = new Channel(perSource);
    const b = new Channel(perSource);

    const pa = produce(a, 0, perSource);
    const pb = produce(b, 1, perSource);
    await Promise.all([pa, pb]);

    let seenA = 0;
    let seenB = 0;
    let checksum = 0;
    let orderOK = true;

    const t0 = performance.now();
    while (seenA + seenB < perSource * 2) {
        const [source, value] = await receivePriority(a, b);
        if (source === 0) {
            if (seenB !== 0 || value !== seenA * 2) {
                orderOK = false;
            }
            checksum = (checksum * 257 + value + 1) % 1000000007;
            seenA++;
        } else {
            if (seenA !== perSource || value !== seenB * 2 + 1) {
                orderOK = false;
            }
            checksum = (checksum * 257 + value + 2) % 1000000007;
            seenB++;
        }
    }
    const t1 = performance.now();

    const expected = expectedChecksum();
    if (orderOK && seenA === perSource && seenB === perSource && checksum === expected) {
        console.log(`count=${seenA + seenB} checksum=${checksum} order=ok`);
    } else {
        console.log(`count=${seenA + seenB} checksum=${checksum} expected=${expected} order=bad`);
    }
    console.log(`bench_ns=${Math.round((t1 - t0) * 1e6)}`);
}

main();

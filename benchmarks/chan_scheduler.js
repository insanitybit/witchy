const { performance } = require('perf_hooks');

class Channel {
    constructor(capacity) {
        this.capacity = capacity;
        this.buffer = [];
        this.receivers = [];
        this.senders = [];
        this.closed = false;
    }

    async send(value) {
        if (this.receivers.length > 0) {
            this.receivers.shift()({ value, done: false });
            return;
        }
        if (this.buffer.length < this.capacity) {
            this.buffer.push(value);
            return;
        }
        await new Promise((resolve) => this.senders.push(() => {
            this.buffer.push(value);
            resolve();
        }));
    }

    close() {
        this.closed = true;
        while (this.receivers.length > 0) {
            this.receivers.shift()({ value: undefined, done: true });
        }
    }

    async receive() {
        if (this.buffer.length > 0) {
            const value = this.buffer.shift();
            if (this.senders.length > 0) this.senders.shift()();
            return { value, done: false };
        }
        if (this.closed) return { value: undefined, done: true };
        return new Promise((resolve) => this.receivers.push(resolve));
    }

    [Symbol.asyncIterator]() {
        return { next: () => this.receive() };
    }
}

async function main() {
    const n = 10_000;
    const t0 = performance.now();
    const ch = new Channel(64);
    const producer = (async () => {
        for (let i = 0; i < n; i++) await ch.send({ left: i, right: i + 1 });
        ch.close();
    })();
    let sum = 0;
    for await (const item of ch) sum += item.left + item.right;
    await producer;
    console.log(sum);
    console.log(`bench_ns=${Math.round((performance.now() - t0) * 1e6)}`);
}

main();

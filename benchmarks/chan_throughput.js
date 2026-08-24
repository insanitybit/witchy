const { performance } = require('perf_hooks');

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

    close() {
        this.closed = true;
        while (this.receivers.length > 0) {
            const resolve = this.receivers.shift();
            resolve({ value: undefined, done: true });
        }
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

    [Symbol.asyncIterator]() {
        return {
            next: () => this.receive()
        };
    }
}

async function main() {
    const n = 64000;
    const t0 = performance.now();
    const ch = new Channel(64);

    const producer = (async () => {
        for (let i = 0; i < n; i++) {
            await ch.send(i);
        }
        ch.close();
    })();

    let sum = 0;
    for await (const v of ch) {
        sum += v;
    }
    await producer;
    const t1 = performance.now();

    console.log(sum);
    console.log(`bench_ns=${Math.round((t1 - t0) * 1e6)}`);
}

main();

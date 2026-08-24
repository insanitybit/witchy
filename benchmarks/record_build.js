const { performance } = require('perf_hooks');

function build(n) {
    const s = { items: [], total: 0 };
    for (let i = 0; i < n; i++) {
        s.items.push(i);
        s.total += i;
    }
    return s;
}

const t0 = performance.now();
const s = build(500000);
console.log(s.total + s.items.length);
console.log(`bench_ns=${Math.round((performance.now() - t0) * 1e6)}`);

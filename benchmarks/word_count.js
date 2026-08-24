const { performance } = require('perf_hooks');

const t0 = performance.now();
const d = new Map();
for (let i = 0; i < 1000000; i++) {
    const w = "word" + (i % 1000);
    d.set(w, (d.get(w) || 0) + 1);
}
let total = 0;
for (const v of d.values()) {
    total += v;
}
console.log(total + d.size);
console.log(`bench_ns=${Math.round((performance.now() - t0) * 1e6)}`);

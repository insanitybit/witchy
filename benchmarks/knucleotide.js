const { performance } = require('perf_hooks');

const t0 = performance.now();
const n = 200000;
const k = 10;
const cs = new Uint8Array(n);
let seed = 42;
const chars = [65, 67, 71, 84]; // A, C, G, T
for (let i = 0; i < n; i++) {
    seed = (seed * 1103515245 + 12345) % 2147483648;
    cs[i] = chars[Math.floor(seed / 65536) % 4];
}
const counts = new Map();
// Simple JS string conversion for map keys is faster than array keys
for (let j = 0; j <= n - k; j++) {
    let key = "";
    for (let l = 0; l < k; l++) {
        key += String.fromCharCode(cs[j + l]);
    }
    counts.set(key, (counts.get(key) || 0) + 1);
}
console.log(counts.size);
console.log(`bench_ns=${Math.round((performance.now() - t0) * 1e6)}`);

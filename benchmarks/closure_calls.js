const { performance } = require('perf_hooks');

const t0 = performance.now();
const f = (x) => x % 7;
let total = 0;
for (let i = 0; i < 5000000; i++) {
    total += f(i);
}
console.log(total);
console.log(`bench_ns=${Math.round((performance.now() - t0) * 1e6)}`);

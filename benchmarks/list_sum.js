const { performance } = require('perf_hooks');

const t0 = performance.now();
const acc = [];
for (let i = 0; i < 3000000; i++) {
    acc.push(i);
}
let total = 0;
for (let i = 0; i < acc.length; i++) {
    total += acc[i];
}
console.log(total);
console.log(`bench_ns=${Math.round((performance.now() - t0) * 1e6)}`);

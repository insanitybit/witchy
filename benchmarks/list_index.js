const { performance } = require('perf_hooks');

const t0 = performance.now();
const xs = new Array(5000);
for (let k = 0; k < 5000; k++) {
    xs[k] = k;
}
let total = 0;
for (let rep = 0; rep < 2000; rep++) {
    for (let i = 0; i < xs.length; i++) {
        total += xs[i];
    }
}
console.log(total);
console.log(`bench_ns=${Math.round((performance.now() - t0) * 1e6)}`);

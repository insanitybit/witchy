const { performance } = require('perf_hooks');

function nsieve(n) {
    const flags = new Uint8Array(n);
    flags.fill(1);
    let count = 0;
    for (let i = 2; i < n; i++) {
        if (flags[i]) {
            count++;
            for (let j = i + i; j < n; j += i) {
                flags[j] = 0;
            }
        }
    }
    return count;
}

const t0 = performance.now();
let total = 0;
total += nsieve(800000);
total += nsieve(400000);
total += nsieve(200000);
console.log(total);
console.log(`bench_ns=${Math.round((performance.now() - t0) * 1e6)}`);

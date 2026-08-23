function steps(start) {
    let n = start;
    let c = 0;
    while (n > 1) {
        if (n % 2 === 0) {
            n = Math.floor(n / 2);
        } else {
            n = 3 * n + 1;
        }
        c += 1;
    }
    return c;
}

const t0 = process.hrtime.bigint();
let total = 0;
for (let i = 1; i < 1000000; i++) {
    total += steps(i);
}
const t1 = process.hrtime.bigint();

console.log(total);
console.log(`bench_ns=${t1 - t0}`);

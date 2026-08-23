function fib(n) {
    if (n < 2) {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

const t0 = process.hrtime.bigint();
const r = fib(35);
const t1 = process.hrtime.bigint();

console.log(r);
console.log(`bench_ns=${t1 - t0}`);

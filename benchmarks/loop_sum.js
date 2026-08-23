const t0 = process.hrtime.bigint();
let s = 0;
let i = 0;
while (i < 100000000) {
    s += i;
    i += 1;
}
const t1 = process.hrtime.bigint();

console.log(s);
console.log(`bench_ns=${t1 - t0}`);

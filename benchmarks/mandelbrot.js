const t0 = process.hrtime.bigint();
let total = 0;
let y = -1.5;
while (y < 1.5) {
    let x = -2.0;
    while (x < 1.0) {
        let zr = 0.0;
        let zi = 0.0;
        let i = 0;
        while (i < 255 && zr * zr + zi * zi <= 4.0) {
            let nzr = zr * zr - zi * zi + x;
            let nzi = 2.0 * zr * zi + y;
            zr = nzr;
            zi = nzi;
            i += 1;
        }
        total += i;
        x += 0.005;
    }
    y += 0.005;
}
const t1 = process.hrtime.bigint();

console.log(total);
console.log(`bench_ns=${t1 - t0}`);

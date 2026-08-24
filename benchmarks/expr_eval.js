const { performance } = require('perf_hooks');

function num(n) { return { kind: 0, n: n }; }
function add(a, b) { return { kind: 1, a: a, b: b }; }
function mul(a, b) { return { kind: 2, a: a, b: b }; }

function evaluate(e) {
    if (e.kind === 0) return e.n;
    if (e.kind === 1) return evaluate(e.a) + evaluate(e.b);
    return evaluate(e.a) * evaluate(e.b);
}

function build(depth) {
    if (depth <= 0) return num(1);
    return add(mul(build(depth - 1), num(2)), build(depth - 1));
}

const t0 = performance.now();
let total = 0;
for (let i = 0; i < 10; i++) {
    total += evaluate(build(16));
}
console.log(total);
console.log(`bench_ns=${Math.round((performance.now() - t0) * 1e6)}`);

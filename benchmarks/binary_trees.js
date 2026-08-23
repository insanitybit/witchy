function build(depth) {
    if (depth === 0) {
        return { left: null, right: null };
    }
    return { left: build(depth - 1), right: build(depth - 1) };
}

function check(node) {
    if (node === null) return 0;
    return 1 + check(node.left) + check(node.right);
}

const t0 = process.hrtime.bigint();
let total = 0;
for (let i = 0; i < 50; i++) {
    const t = build(16);
    total += check(t);
}
const t1 = process.hrtime.bigint();

console.log(total);
console.log(`bench_ns=${t1 - t0}`);

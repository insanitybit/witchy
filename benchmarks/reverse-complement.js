const alu = "GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGGGAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGACCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAATACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCAGCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGGAGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCCAGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA";

const total = 3000000;

const seq = Buffer.allocUnsafe(total);
const aluBuf = Buffer.from(alu);
for (let i = 0; i < total; i++) {
    seq[i] = aluBuf[i % aluBuf.length];
}

const complement = Buffer.allocUnsafe(256);
for (let i = 0; i < 256; i++) complement[i] = i;
complement['A'.charCodeAt(0)] = 'T'.charCodeAt(0);
complement['a'.charCodeAt(0)] = 'T'.charCodeAt(0);
complement['C'.charCodeAt(0)] = 'G'.charCodeAt(0);
complement['c'.charCodeAt(0)] = 'G'.charCodeAt(0);
complement['G'.charCodeAt(0)] = 'C'.charCodeAt(0);
complement['g'.charCodeAt(0)] = 'C'.charCodeAt(0);
complement['T'.charCodeAt(0)] = 'A'.charCodeAt(0);
complement['t'.charCodeAt(0)] = 'A'.charCodeAt(0);

const t0 = process.hrtime.bigint();

for (let iter = 0; iter < 15; iter++) {
    let i = 0;
    let j = total - 1;
    while (i <= j) {
        const ci = seq[i];
        const cj = seq[j];
        seq[i] = complement[cj];
        seq[j] = complement[ci];
        i++;
        j--;
    }
}

const t1 = process.hrtime.bigint();

console.log(">ONE Homo sapiens alu");
for (let k = 0; k < total; k += 60) {
    let end = k + 60;
    if (end > total) end = total;
    console.log(seq.subarray(k, end).toString());
}
console.log(`bench_ns=${Number(t1 - t0)}`);

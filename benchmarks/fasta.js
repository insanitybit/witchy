const fs = require('fs');

const t0 = process.hrtime.bigint();
const out = process.stdout;

let last = 42;
const A = 3877;
const C = 29573;
const M = 139968;

function rand(max) {
    last = (last * A + C) % M;
    return max * last / M;
}

const ALU =
    "GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGG" +
    "GAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGA" +
    "CCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAAT" +
    "ACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCA" +
    "GCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGG" +
    "AGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCC" +
    "AGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA";

const IUB = [
    [0.27, 'a'], [0.12, 'c'], [0.12, 'g'], [0.27, 't'],
    [0.02, 'B'], [0.02, 'D'], [0.02, 'H'], [0.02, 'K'],
    [0.02, 'M'], [0.02, 'N'], [0.02, 'R'], [0.02, 'S'],
    [0.02, 'V'], [0.02, 'W'], [0.02, 'Y']
];

const HomoSap = [
    [0.3029549426680, 'a'],
    [0.1979883004921, 'c'],
    [0.1975473066391, 'g'],
    [0.3015094502008, 't']
];

function makeCumulative(table) {
    let cp = 0.0;
    for (let i = 0; i < table.length; i++) {
        cp += table[i][0];
        table[i][0] = cp;
    }
}

makeCumulative(IUB);
makeCumulative(HomoSap);

function fastaRepeat(n, seq) {
    let seqi = 0;
    let lenOut = 60;
    while (n > 0) {
        if (n < lenOut) lenOut = n;
        if (seqi + lenOut < seq.length) {
            out.write(seq.substring(seqi, seqi + lenOut) + '\n');
            seqi += lenOut;
        } else {
            let s = seq.substring(seqi);
            seqi = lenOut - s.length;
            out.write(s + seq.substring(0, seqi) + '\n');
        }
        n -= lenOut;
    }
}

function fastaRandom(n, table) {
    let line = Buffer.allocUnsafe(60);
    while (n > 0) {
        let lenOut = 60;
        if (n < lenOut) {
            lenOut = n;
            line = Buffer.allocUnsafe(lenOut);
        }
        for (let i = 0; i < lenOut; i++) {
            let r = rand(1);
            for (let j = 0; j < table.length; j++) {
                if (r < table[j][0]) {
                    line[i] = table[j][1].charCodeAt(0);
                    break;
                }
            }
        }
        out.write(line);
        out.write('\n');
        n -= lenOut;
    }
}

const n = 2500000;

out.write(">ONE Homo sapiens alu\n");
fastaRepeat(2 * n, ALU);

out.write(">TWO IUB ambiguity codes\n");
fastaRandom(3 * n, IUB);

out.write(">THREE Homo sapiens frequency\n");
fastaRandom(5 * n, HomoSap);

const t1 = process.hrtime.bigint();
out.write(`bench_ns=${t1 - t0}\n`);

const fs = require('fs');

const t0 = process.hrtime.bigint();

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

const NL = Buffer.from('\n');

function fastaRepeat(n, seq) {
    let seqi = 0;
    let lenOut = 60;
    while (n > 0) {
        if (n < lenOut) lenOut = n;
        if (seqi + lenOut < seq.length) {
            fs.writeSync(1, seq.substring(seqi, seqi + lenOut) + '\n');
            seqi += lenOut;
        } else {
            let s = seq.substring(seqi);
            seqi = lenOut - s.length;
            fs.writeSync(1, s + seq.substring(0, seqi) + '\n');
        }
        n -= lenOut;
    }
}

function fastaRandom(n, table) {
    let line = Buffer.allocUnsafe(61);
    line[60] = 10; // '\n'
    while (n > 0) {
        let lenOut = 60;
        if (n < lenOut) {
            lenOut = n;
            line = Buffer.allocUnsafe(lenOut + 1);
            line[lenOut] = 10;
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
        fs.writeSync(1, line.subarray(0, lenOut + 1));
        n -= lenOut;
    }
}

const n = 2500000;

fs.writeSync(1, ">ONE Homo sapiens alu\n");
fastaRepeat(2 * n, ALU);

fs.writeSync(1, ">TWO IUB ambiguity codes\n");
fastaRandom(3 * n, IUB);

fs.writeSync(1, ">THREE Homo sapiens frequency\n");
fastaRandom(5 * n, HomoSap);

const t1 = process.hrtime.bigint();
fs.writeSync(1, `bench_ns=${t1 - t0}\n`);

let t0 = Date.now();

const seq = "agggtaaa cgtgggtaaa aactggtaaa agactgtaaa aggacttaaa agggcgaaa agggtcaa agggtaca agggtaac aatggtaaa agagtaaa aggataaa agggcaaa tHaNt aND caN HaD WaS aNt BY <header> |word| ";

let sb = "";
for (let i = 0; i < 250; i++) {
    sb += ">Sequence_" + i + "\n" + seq + "\n";
}
let text = sb;
let originalLen = Buffer.byteLength(text, 'utf8');

text = text.replace(/(>[^\n]+)?\n/g, "");
let cleanedLen = Buffer.byteLength(text, 'utf8');

const variants = [
    /agggtaaa|tttaccct/g,
    /[cgt]gggtaaa|tttaccc[acg]/g,
    /a[act]ggtaaa|tttacc[agt]t/g,
    /ag[act]gtaaa|tttac[agt]ct/g,
    /agg[act]taaa|ttta[agt]cct/g,
    /aggg[acg]aaa|ttt[cgt]ccct/g,
    /agggt[cgt]aa|tt[acg]accct/g,
    /agggta[cgt]a|t[acg]taccct/g,
    /agggtaa[cgt]|[acg]ttaccct/g
];

for (let i = 0; i < variants.length; i++) {
    const re = variants[i];
    const match = text.match(re);
    const count = match ? match.length : 0;
    // get string rep of regex without flags
    let reStr = re.toString();
    reStr = reStr.substring(1, reStr.length - 2); 
    console.log(reStr + " " + count);
}

const substitutions = [
    [/tHa[Nt]/g, "<4>"],
    [/aND|caN|Ha[DS]|WaS/g, "<3>"],
    [/a[NSt]|BY/g, "<2>"],
    [/<[^>]*>/g, "|"],
    [/\|[^|][^|]*\|/g, "-"]
];

for (let i = 0; i < substitutions.length; i++) {
    text = text.replace(substitutions[i][0], substitutions[i][1]);
}

console.log("\n" + originalLen + "\n" + cleanedLen + "\n" + Buffer.byteLength(text, 'utf8'));
console.log("bench_ns=" + ((Date.now() - t0) * 1000000));

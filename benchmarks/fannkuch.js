const { performance } = require('perf_hooks');

function fannkuch(n) {
    const perm1 = new Int32Array(n);
    for (let i = 0; i < n; i++) perm1[i] = i;
    const count = new Int32Array(n);
    const perm = new Int32Array(n);
    let maxFlips = 0;
    let checksum = 0;
    let permCount = 0;
    let r = n;
    
    while (true) {
        while (r !== 1) {
            count[r - 1] = r;
            r--;
        }
        if (perm1[0] !== 0) {
            for (let i = 0; i < n; i++) perm[i] = perm1[i];
            let flips = 0;
            let k = perm[0];
            while (k !== 0) {
                let lo = 0, hi = k;
                while (lo < hi) {
                    const t = perm[lo];
                    perm[lo] = perm[hi];
                    perm[hi] = t;
                    lo++; hi--;
                }
                flips++;
                k = perm[0];
            }
            if (flips > maxFlips) maxFlips = flips;
            if (permCount % 2 === 0) checksum += flips;
            else checksum -= flips;
        }
        let advanced = false;
        while (!advanced) {
            if (r === n) {
                console.log(checksum * 1000 + maxFlips);
                return maxFlips;
            }
            const perm0 = perm1[0];
            for (let i = 0; i < r; i++) perm1[i] = perm1[i + 1];
            perm1[r] = perm0;
            count[r]--;
            if (count[r] > 0) advanced = true;
            else r++;
        }
        permCount++;
    }
}

const t0 = performance.now();
fannkuch(10);
console.log(`bench_ns=${Math.round((performance.now() - t0) * 1e6)}`);

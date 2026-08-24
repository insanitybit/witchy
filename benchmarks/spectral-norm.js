function A(i, j) {
    return 1.0 / (((i + j) * (i + j + 1)) / 2 + i + 1);
}

function multiplyAv(v, Atv) {
    let n = v.length;
    for (let i = 0; i < n; i++) {
        let sum = 0.0;
        for (let j = 0; j < n; j++) {
            sum += A(i, j) * v[j];
        }
        Atv[i] = sum;
    }
}

function multiplyAtv(v, Atv) {
    let n = v.length;
    for (let i = 0; i < n; i++) {
        let sum = 0.0;
        for (let j = 0; j < n; j++) {
            sum += A(j, i) * v[j];
        }
        Atv[i] = sum;
    }
}

function multiplyAtAv(v, tmp, AtAv) {
    multiplyAv(v, tmp);
    multiplyAtv(tmp, AtAv);
}

function main() {
    let n = 1000;
    
    let u = new Float64Array(n);
    let v = new Float64Array(n);
    let tmp = new Float64Array(n);
    
    for (let i = 0; i < n; i++) {
        u[i] = 1.0;
    }
    
    let t0 = process.hrtime.bigint();
    
    for (let i = 0; i < 10; i++) {
        multiplyAtAv(u, tmp, v);
        multiplyAtAv(v, tmp, u);
    }
    
    let vBv = 0.0;
    let vv = 0.0;
    for (let i = 0; i < n; i++) {
        vBv += u[i] * v[i];
        vv += v[i] * v[i];
    }
    
    let res = Math.sqrt(vBv / vv);
    let t1 = process.hrtime.bigint();
    
    console.log(res.toFixed(9));
    console.log("bench_ns=" + (t1 - t0).toString());
}
main();

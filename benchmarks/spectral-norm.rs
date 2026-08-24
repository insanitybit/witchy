use std::time::Instant;

fn a(i: usize, j: usize) -> f64 {
    1.0 / (((i + j) * (i + j + 1) / 2 + i + 1) as f64)
}

fn multiply_av(v: &[f64], atv: &mut [f64]) {
    for i in 0..v.len() {
        let mut sum = 0.0;
        for j in 0..v.len() {
            sum += a(i, j) * v[j];
        }
        atv[i] = sum;
    }
}

fn multiply_atv(v: &[f64], atv: &mut [f64]) {
    for i in 0..v.len() {
        let mut sum = 0.0;
        for j in 0..v.len() {
            sum += a(j, i) * v[j];
        }
        atv[i] = sum;
    }
}

fn multiply_at_av(v: &[f64], tmp: &mut [f64], at_av: &mut [f64]) {
    multiply_av(v, tmp);
    multiply_atv(tmp, at_av);
}

fn main() {
    let n = 1000;
    let t0 = Instant::now();
    
    let mut u = vec![1.0; n];
    let mut v = vec![0.0; n];
    let mut tmp = vec![0.0; n];
    
    for _ in 0..10 {
        multiply_at_av(&u, &mut tmp, &mut v);
        multiply_at_av(&v, &mut tmp, &mut u);
    }
    
    let mut v_bv = 0.0;
    let mut vv = 0.0;
    for i in 0..n {
        v_bv += u[i] * v[i];
        vv += v[i] * v[i];
    }
    
    let res = (v_bv / vv).sqrt();
    let duration = t0.elapsed();
    
    println!("{:.9}", res);
    println!("bench_ns={}", duration.as_nanos());
}

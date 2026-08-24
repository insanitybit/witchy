use std::time::Instant;

fn main() {
    let t0 = Instant::now();
    let mut xs = Vec::with_capacity(5000);
    for k in 0..5000 {
        xs.push(k as i64);
    }
    let mut total: i64 = 0;
    for _ in 0..2000 {
        for i in 0..xs.len() {
            total += xs[i];
        }
    }
    println!("{}", total);
    println!("bench_ns={}", t0.elapsed().as_nanos());
}

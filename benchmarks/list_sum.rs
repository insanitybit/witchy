use std::time::Instant;

fn main() {
    let t0 = Instant::now();
    let mut acc: Vec<i64> = Vec::new();
    let mut i: i64 = 0;
    while i < 3000000 {
        acc.push(i);
        i += 1;
    }
    let mut total: i64 = 0;
    for x in acc {
        total += x;
    }
    println!("{}", total);
    println!("bench_ns={}", t0.elapsed().as_nanos());
}

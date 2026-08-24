use std::time::Instant;

fn main() {
    let t0 = Instant::now();
    let mut acc = Vec::new();
    let mut i = 0;
    while i < 3000000 {
        acc.push(i);
        i += 1;
    }
    let mut total = 0;
    for x in acc {
        total += x;
    }
    println!("{}", total);
    println!("bench_ns={}", t0.elapsed().as_nanos());
}

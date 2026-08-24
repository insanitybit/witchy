use std::time::Instant;
use std::collections::HashMap;

fn main() {
    let t0 = Instant::now();
    let mut d: HashMap<i32, i32> = HashMap::new();
    let mut i = 0;
    while i < 3000000 {
        let k = i % 1000;
        *d.entry(k).or_insert(0) += 1;
        i += 1;
    }
    let mut total = 0;
    for v in d.values() {
        total += v;
    }
    println!("{}", total);
    println!("bench_ns={}", t0.elapsed().as_nanos());
}

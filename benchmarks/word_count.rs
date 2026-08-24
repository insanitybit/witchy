use std::time::Instant;
use std::collections::HashMap;

fn main() {
    let t0 = Instant::now();
    let mut d: HashMap<String, i32> = HashMap::new();
    for i in 0..1000000 {
        let w = format!("word{}", i % 1000);
        *d.entry(w).or_insert(0) += 1;
    }
    let mut total = 0;
    for v in d.values() {
        total += v;
    }
    println!("{}", total + d.len() as i32);
    println!("bench_ns={}", t0.elapsed().as_nanos());
}

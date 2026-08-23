fn main() {
    let t0 = std::time::Instant::now();
    let mut s: i64 = 0;
    let mut i: i64 = 0;
    while i < 100000000 {
        s = s.wrapping_add(i);
        i += 1;
    }
    let duration = t0.elapsed();
    println!("{}", s);
    println!("bench_ns={}", duration.as_nanos());
}

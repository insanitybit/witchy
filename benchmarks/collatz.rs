fn steps(start: i64) -> i64 {
    let mut n = start;
    let mut c = 0;
    while n > 1 {
        if n % 2 == 0 {
            n = n / 2;
        } else {
            n = 3 * n + 1;
        }
        c += 1;
    }
    c
}

fn main() {
    let t0 = std::time::Instant::now();
    let mut total = 0;
    let mut i = 1;
    while i < 1000000 {
        total += steps(i);
        i += 1;
    }
    let duration = t0.elapsed();
    println!("{}", total);
    println!("bench_ns={}", duration.as_nanos());
}

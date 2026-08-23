fn fib(n: i64) -> i64 {
    if n < 2 {
        n
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

fn main() {
    let t0 = std::time::Instant::now();
    let res = fib(35);
    let duration = t0.elapsed();
    println!("{}", res);
    println!("bench_ns={}", duration.as_nanos());
}

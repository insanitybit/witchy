use std::time::Instant;

fn main() {
    let t0 = Instant::now();
    let f = |x: i32| x % 7;
    let mut total = 0;
    for i in 0..5000000 {
        total += f(i);
    }
    println!("{}", total);
    println!("bench_ns={}", t0.elapsed().as_nanos());
}

use std::hint::black_box;
use std::time::Instant;

#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn witchy_rust_class_kernel() -> i64 {
    let count = black_box(300_000_i64);
    let mut values = Vec::with_capacity(count as usize);
    for i in 0..count {
        values.push(i * 3);
    }
    let mut total = 0_i64;
    for value in values {
        if value % 5 != 0 {
            total += (value * 7 + 11) % 101;
        }
    }
    black_box(total)
}

fn main() {
    let start = Instant::now();
    let result = witchy_rust_class_kernel();
    let elapsed = start.elapsed().as_nanos();
    println!("result={result}");
    println!("bench_ns={elapsed}");
}

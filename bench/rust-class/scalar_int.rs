use std::hint::black_box;
use std::time::Instant;

#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn witchy_rust_class_kernel() -> i64 {
    let count = black_box(2000_i64);
    let mut total = 0_i64;
    for i in 0_i64..count {
        for j in 0_i64..count {
            total += (i * 7 + j) % 13;
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

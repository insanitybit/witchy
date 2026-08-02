use std::hint::black_box;
use std::time::Instant;

#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn witchy_rust_class_kernel() -> i64 {
    let mut value = black_box(1.0_f64);
    let scale = black_box(1.000_000_1_f64);
    let increment = black_box(0.000_000_3_f64);
    let reset_at = black_box(1.1_f64);
    let mut checksum = 0_i64;
    for _ in 0..1_000_000 {
        value = value * scale + increment;
        if value > reset_at {
            checksum += 1;
            value *= 0.5;
        }
    }
    black_box(checksum)
}

fn main() {
    let start = Instant::now();
    let result = witchy_rust_class_kernel();
    let elapsed = start.elapsed().as_nanos();
    println!("result={result}");
    println!("bench_ns={elapsed}");
}

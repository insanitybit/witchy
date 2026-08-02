use std::hint::black_box;
use std::time::Instant;

#[derive(Clone, Copy)]
struct Pair {
    left: i64,
    right: i64,
}

#[inline(never)]
fn build(value: i64) -> Pair {
    Pair {
        left: value * 7 + 3,
        right: value * 11 + 5,
    }
}

#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn witchy_rust_class_kernel() -> i64 {
    let count = black_box(1_000_000_i64);
    let mut current = build(0);
    let mut total = 0_i64;
    for i in 1..count {
        current = build(i);
        total += (current.left + current.right) % 101;
    }
    black_box(total + current.left % 17)
}

fn main() {
    let start = Instant::now();
    let result = witchy_rust_class_kernel();
    let elapsed = start.elapsed().as_nanos();
    println!("result={result}");
    println!("bench_ns={elapsed}");
}

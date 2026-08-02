use std::hint::black_box;
use std::time::Instant;

#[derive(Clone, Copy)]
struct Point {
    x: i64,
    y: i64,
}

#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn witchy_rust_class_kernel() -> i64 {
    let count = black_box(200_000_i64);
    let mut points = Vec::with_capacity(count as usize);
    for i in 0..count {
        points.push(Point { x: i, y: i * 3 });
    }
    let mut total = 0_i64;
    for point in points {
        total += (point.x * 7 + point.y) % 97;
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

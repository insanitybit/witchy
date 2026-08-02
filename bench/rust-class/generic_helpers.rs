use std::hint::black_box;
use std::time::Instant;

#[derive(Clone, Copy)]
struct Point {
    x: i64,
    y: i64,
}

#[inline(never)]
fn identity<T>(value: T) -> T {
    value
}

#[inline(never)]
fn weight(point: Point) -> i64 {
    point.x * 5 + point.y * 3
}

#[inline(never)]
fn sum_points(points: Vec<Point>) -> i64 {
    let mut total = 0_i64;
    for point in points {
        total += weight(identity(point)) % 97;
    }
    total
}

#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn witchy_rust_class_kernel() -> i64 {
    let count = black_box(200_000_i64);
    let mut points = Vec::with_capacity(count as usize);
    for i in 0..count {
        points.push(Point { x: i, y: i * 2 + 1 });
    }
    black_box(sum_points(points))
}

fn main() {
    let start = Instant::now();
    let result = witchy_rust_class_kernel();
    let elapsed = start.elapsed().as_nanos();
    println!("result={result}");
    println!("bench_ns={elapsed}");
}

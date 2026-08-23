fn main() {
    let t0 = std::time::Instant::now();
    let mut total = 0;
    let mut y = -1.5;
    while y < 1.5 {
        let mut x = -2.0;
        while x < 1.0 {
            let mut zr: f64 = 0.0;
            let mut zi: f64 = 0.0;
            let mut i = 0;
            while i < 255 && zr * zr + zi * zi <= 4.0 {
                let nzr = zr * zr - zi * zi + x;
                let nzi = 2.0 * zr * zi + y;
                zr = nzr;
                zi = nzi;
                i += 1;
            }
            total += i;
            x += 0.005;
        }
        y += 0.005;
    }
    let duration = t0.elapsed();
    println!("{}", total);
    println!("bench_ns={}", duration.as_nanos());
}

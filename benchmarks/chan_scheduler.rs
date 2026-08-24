use std::sync::mpsc;
use std::thread;
use std::time::Instant;

fn main() {
    let n = 10_000;
    let t0 = Instant::now();
    let (tx, rx) = mpsc::sync_channel(64);
    thread::spawn(move || {
        for i in 0..n {
            tx.send((i, i + 1)).unwrap();
        }
    });
    let mut sum = 0;
    for (left, right) in rx {
        sum += left + right;
    }
    println!("{}", sum);
    println!("bench_ns={}", t0.elapsed().as_nanos());
}

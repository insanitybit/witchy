use std::sync::mpsc;
use std::thread;
use std::time::Instant;

fn main() {
    let n = 64000;
    let t0 = Instant::now();
    let (tx, rx) = mpsc::sync_channel(64);
    
    thread::spawn(move || {
        for i in 0..n {
            tx.send(i).unwrap();
        }
    });
    
    let mut sum = 0;
    for v in rx {
        sum += v;
    }
    let elapsed = t0.elapsed();
    println!("{}", sum);
    println!("bench_ns={}", elapsed.as_nanos());
}

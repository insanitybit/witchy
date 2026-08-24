use std::sync::mpsc;
use std::thread;

fn main() {
    let n = 64000;
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
    println!("{}", sum);
}

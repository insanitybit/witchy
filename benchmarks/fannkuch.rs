use std::time::Instant;

fn fannkuch(n: usize) -> i32 {
    let mut perm1: Vec<i32> = (0..n as i32).collect();
    let mut count = vec![0; n];
    let mut perm = vec![0; n];
    let mut max_flips = 0;
    let mut checksum = 0;
    let mut perm_count = 0;
    let mut r = n;

    loop {
        while r != 1 {
            count[r - 1] = r as i32;
            r -= 1;
        }

        if perm1[0] != 0 {
            perm.copy_from_slice(&perm1);
            let mut flips = 0;
            let mut k = perm[0] as usize;
            
            while k != 0 {
                let mut lo = 0;
                let mut hi = k;
                while lo < hi {
                    perm.swap(lo, hi);
                    lo += 1;
                    hi -= 1;
                }
                flips += 1;
                k = perm[0] as usize;
            }
            
            if flips > max_flips {
                max_flips = flips;
            }
            
            if perm_count % 2 == 0 {
                checksum += flips;
            } else {
                checksum -= flips;
            }
        }

        let mut advanced = false;
        while !advanced {
            if r == n {
                println!("{}", checksum * 1000 + max_flips);
                return max_flips;
            }
            
            let perm0 = perm1[0];
            for i in 0..r {
                perm1[i] = perm1[i + 1];
            }
            perm1[r] = perm0;
            count[r] -= 1;
            
            if count[r] > 0 {
                advanced = true;
            } else {
                r += 1;
            }
        }
        perm_count += 1;
    }
}

fn main() {
    let t0 = Instant::now();
    fannkuch(10);
    println!("bench_ns={}", t0.elapsed().as_nanos());
}

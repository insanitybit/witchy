package main

import (
	"fmt"
	"time"
)

func fannkuch(n int) int {
	perm1 := make([]int, n)
	for i := range perm1 {
		perm1[i] = i
	}
	count := make([]int, n)
	perm := make([]int, n)
	maxFlips := 0
	checksum := 0
	permCount := 0
	r := n
	for {
		for r != 1 {
			count[r-1] = r
			r--
		}
		if perm1[0] != 0 {
			copy(perm, perm1)
			flips := 0
			k := perm[0]
			for k != 0 {
				lo, hi := 0, k
				for lo < hi {
					perm[lo], perm[hi] = perm[hi], perm[lo]
					lo++
					hi--
				}
				flips++
				k = perm[0]
			}
			if flips > maxFlips {
				maxFlips = flips
			}
			if permCount%2 == 0 {
				checksum += flips
			} else {
				checksum -= flips
			}
		}
		advanced := false
		for !advanced {
			if r == n {
				fmt.Println(checksum*1000 + maxFlips)
				return maxFlips
			}
			perm0 := perm1[0]
			for i := 0; i < r; i++ {
				perm1[i] = perm1[i+1]
			}
			perm1[r] = perm0
			count[r]--
			if count[r] > 0 {
				advanced = true
			} else {
				r++
			}
		}
		permCount++
	}
}

func main() {
	t0 := time.Now()
	fannkuch(10)
	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

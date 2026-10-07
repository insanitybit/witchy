package main

import (
	"fmt"
	"time"
)

func residue(x int64) int64 { return x % 7 }

func invoke(f func(int64) int64, x int64) int64 { return f(x) }

func main() {
	start := time.Now()
	var total int64
	for i := int64(0); i < 2000000; i++ {
		total += invoke(residue, i)
	}
	finish := time.Now()
	fmt.Println(total)
	fmt.Printf("bench_ns=%d\n", finish.Sub(start).Nanoseconds())
}

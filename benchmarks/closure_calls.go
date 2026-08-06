package main

import (
	"fmt"
	"time"
)

func main() {
	t0 := time.Now()
	f := func(x int) int { return x % 7 }
	total := 0
	for i := 0; i < 5000000; i++ {
		total += f(i)
	}
	fmt.Println(total)
	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

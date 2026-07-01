package main

import (
	"fmt"
	"time"
)

func fib(n int) int {
	if n < 2 {
		return n
	}
	return fib(n-1) + fib(n-2)
}

func main() {
	t0 := time.Now()
	fmt.Println(fib(35))
	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

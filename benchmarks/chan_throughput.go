package main

import (
	"fmt"
	"time"
)

func main() {
	const n = 64000
	t0 := time.Now()
	ch := make(chan int, 64)
	go func() {
		for i := 0; i < n; i++ {
			ch <- i
		}
		close(ch)
	}()
	sum := 0
	for v := range ch {
		sum += v
	}
	t1 := time.Now()
	fmt.Println(sum)
	fmt.Printf("bench_ns=%d\n", t1.Sub(t0).Nanoseconds())
}

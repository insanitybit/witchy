package main

import (
	"fmt"
	"time"
)

type packet struct {
	left  int
	right int
}

func main() {
	const n = 10000
	t0 := time.Now()
	ch := make(chan packet, 64)
	go func() {
		for i := 0; i < n; i++ {
			ch <- packet{i, i + 1}
		}
		close(ch)
	}()
	sum := 0
	for item := range ch {
		sum += item.left + item.right
	}
	t1 := time.Now()
	fmt.Println(sum)
	fmt.Printf("bench_ns=%d\n", t1.Sub(t0).Nanoseconds())
}

package main

import (
	"fmt"
	"time"
)

func main() {
	t0 := time.Now()
	s := 0
	i := 0
	for i < 100000000 {
		s = s + i
		i = i + 1
	}
	fmt.Println(s)
	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

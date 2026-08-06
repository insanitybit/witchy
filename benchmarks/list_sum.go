package main

import (
	"fmt"
	"time"
)

func main() {
	t0 := time.Now()
	acc := []int{}
	i := 0
	for i < 3000000 {
		acc = append(acc, i)
		i = i + 1
	}
	total := 0
	for _, x := range acc {
		total = total + x
	}
	fmt.Println(total)
	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

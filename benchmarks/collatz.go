package main

import (
	"fmt"
	"time"
)

func steps(start int) int {
	n := start
	c := 0
	for n > 1 {
		if n%2 == 0 {
			n = n / 2
		} else {
			n = 3*n + 1
		}
		c = c + 1
	}
	return c
}

func main() {
	t0 := time.Now()
	total := 0
	i := 1
	for i < 1000000 {
		total = total + steps(i)
		i = i + 1
	}
	fmt.Println(total)
	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

package main

import (
	"fmt"
	"time"
)

func main() {
	t0 := time.Now()
	d := map[string]int{}
	for i := 0; i < 1000000; i++ {
		w := fmt.Sprintf("word%d", i%1000)
		d[w] = d[w] + 1
	}
	total := 0
	for _, v := range d {
		total += v
	}
	fmt.Println(total + len(d))
	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

package main

import (
	"fmt"
	"time"
)

func main() {
	t0 := time.Now()
	total := 0
	y := -1.5
	for y < 1.5 {
		x := -2.0
		for x < 1.0 {
			zr := 0.0
			zi := 0.0
			i := 0
			for i < 255 && zr*zr+zi*zi <= 4.0 {
				nzr := zr*zr - zi*zi + x
				nzi := 2.0*zr*zi + y
				zr = nzr
				zi = nzi
				i = i + 1
			}
			total = total + i
			x = x + 0.005
		}
		y = y + 0.005
	}
	fmt.Println(total)
	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

package main

import (
	"fmt"
	"time"
)

// Deliberately the same naive accumulate form as the witchy program (not
// strings.Builder) — the comparison is what each compiler does with it.
func main() {
	t0 := time.Now()
	s := ""
	for i := 0; i < 20000; i++ {
		s += "ab"
	}
	fmt.Println(len(s))
	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

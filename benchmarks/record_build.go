package main

import (
	"fmt"
	"time"
)

type Stack struct {
	items []int
	total int
}

func build(n int) Stack {
	s := Stack{items: []int{}, total: 0}
	i := 0
	for i < n {
		s.items = append(s.items, i)
		s.total = s.total + i
		i = i + 1
	}
	return s
}

func main() {
	t0 := time.Now()
	s := build(500000)
	fmt.Println(s.total + len(s.items))
	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

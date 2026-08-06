package main

import (
	"fmt"
	"time"
)

type Tree struct {
	l, r *Tree
}

func build(depth int) *Tree {
	if depth == 0 {
		return &Tree{nil, nil}
	}
	return &Tree{build(depth - 1), build(depth - 1)}
}

func check(t *Tree) int {
	if t == nil {
		return 0
	}
	return 1 + check(t.l) + check(t.r)
}

func main() {
	t0 := time.Now()
	total := 0
	for i := 0; i < 50; i++ {
		t := build(16)
		total += check(t)
	}
	fmt.Println(total)
	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

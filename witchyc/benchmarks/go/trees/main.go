package main

import "fmt"

type Tree struct{ children []*Tree }

func build(d int) *Tree {
	if d == 0 { return &Tree{} }
	return &Tree{children: []*Tree{build(d - 1), build(d - 1)}}
}
func check(t *Tree) int {
	c := 1
	for _, ch := range t.children { c += check(ch) }
	return c
}
func main() {
	n := 16
	fmt.Printf("stretch tree of depth %d\t check: %d\n", n+1, check(build(n+1)))
	long := build(n)
	for d := 4; d <= n; d += 2 {
		it := 1 << (n - d + 4)
		total := 0
		for i := 0; i < it; i++ { total += check(build(d)) }
		fmt.Printf("%d\t trees of depth %d\t check: %d\n", it, d, total)
	}
	fmt.Printf("long lived tree of depth %d\t check: %d\n", n, check(long))
}

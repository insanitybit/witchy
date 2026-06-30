package main

import (
	"fmt"
	"sync"
)

func busy(n int) int {
	acc := 0
	for i := 0; i < 40000000; i++ {
		acc += (n * i) % 7
	}
	return acc
}

func main() {
	xs := []int{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16}
	ys := make([]int, len(xs))
	var wg sync.WaitGroup
	for idx, n := range xs {
		wg.Add(1)
		go func(idx, n int) { defer wg.Done(); ys[idx] = busy(n) }(idx, n)
	}
	wg.Wait()
	sum := 0
	for _, y := range ys {
		sum += y
	}
	fmt.Println(sum)
}

package main

import "fmt"

func main() {
	var xs []int64
	for i := int64(0); i < 300000; i++ {
		xs = append(xs, i*3)
	}
	var total int64
	for _, x := range xs {
		total += x % 7
	}
	fmt.Println(total)
}

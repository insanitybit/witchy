package main

import "fmt"

func main() {
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
}

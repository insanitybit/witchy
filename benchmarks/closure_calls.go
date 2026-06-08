package main

import "fmt"

func main() {
	f := func(x int) int { return x % 7 }
	total := 0
	for i := 0; i < 5000000; i++ {
		total += f(i)
	}
	fmt.Println(total)
}

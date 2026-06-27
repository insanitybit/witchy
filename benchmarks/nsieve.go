package main

import "fmt"

func nsieve(n int) int {
	flags := make([]bool, n)
	for i := range flags {
		flags[i] = true
	}
	count := 0
	for i := 2; i < n; i++ {
		if flags[i] {
			count++
			for j := i + i; j < n; j += i {
				flags[j] = false
			}
		}
	}
	return count
}

func main() {
	total := 0
	total += nsieve(800000)
	total += nsieve(400000)
	total += nsieve(200000)
	fmt.Println(total)
}

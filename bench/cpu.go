package main

import "fmt"

func main() {
	var total int64
	for i := int64(0); i < 2000; i++ {
		for j := int64(0); j < 2000; j++ {
			total += (i*7 + j) % 13
		}
	}
	fmt.Println(total)
}

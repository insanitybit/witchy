package main

import "fmt"

func main() {
	s := 0
	i := 0
	for i < 100000000 {
		s = s + i
		i = i + 1
	}
	fmt.Println(s)
}

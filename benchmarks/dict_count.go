package main

import "fmt"

func main() {
	d := map[int]int{}
	i := 0
	for i < 3000000 {
		k := i % 1000
		d[k] = d[k] + 1
		i = i + 1
	}
	total := 0
	for _, v := range d {
		total = total + v
	}
	fmt.Println(total)
}

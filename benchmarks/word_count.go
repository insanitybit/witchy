package main

import "fmt"

func main() {
	d := map[string]int{}
	for i := 0; i < 1000000; i++ {
		w := fmt.Sprintf("word%d", i%1000)
		d[w] = d[w] + 1
	}
	total := 0
	for _, v := range d {
		total += v
	}
	fmt.Println(total + len(d))
}

// Go equivalent of list_index.witchy: build a 5000-element slice, then sum it
// by index 2000 times (10M indexed reads). Go elides the bounds check here via
// its own BCE; the witchy `bounds-elide` lever is the analogous pass.
package main

import "fmt"

func main() {
	xs := make([]int64, 0, 5000)
	for k := 0; k < 5000; k++ {
		xs = append(xs, int64(k))
	}
	var total int64
	for rep := 0; rep < 2000; rep++ {
		for i := 0; i < len(xs); i++ {
			total += xs[i]
		}
	}
	fmt.Println(total)
}

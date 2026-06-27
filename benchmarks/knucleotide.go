package main

import "fmt"

func main() {
	n := 200000
	k := 10
	cs := make([]byte, n)
	seed := int64(42)
	for i := 0; i < n; i++ {
		seed = (seed*1103515245 + 12345) % 2147483648
		cs[i] = "ACGT"[(seed/65536)%4]
	}
	counts := make(map[string]int)
	for j := 0; j+k <= n; j++ {
		counts[string(cs[j:j+k])]++
	}
	fmt.Println(len(counts))
}

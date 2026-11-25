package main

import "fmt"

func main() {
	m := make(map[uint64]uint64, 500000)
	seed := uint64(42)
	for i := 0; i < 1000000; i++ { seed = seed * 48271 % 2147483647; m[seed%500000]++ }
	hits, total := 0, uint64(0)
	for k := uint64(0); k < 1000000; k++ { if c, ok := m[k]; ok { hits++; total += c } }
	fmt.Println(len(m)); fmt.Println(hits); fmt.Println(total)
}

package main

import ("fmt"; "strings")

func main() {
	var b strings.Builder
	seed := uint64(7)
	for i := 0; i < 1200000; i++ {
		seed = seed * 48271 % 2147483647
		w := seed % 1000
		b.WriteByte(byte(97 + w%26)); b.WriteByte(byte(97 + w/26%26)); b.WriteByte(byte(97 + w/676)); b.WriteByte(32)
	}
	counts := make(map[string]uint64, 2048)
	words := uint64(0)
	for _, word := range strings.Fields(b.String()) {
		words++
		counts[word]++
	}
	fmt.Printf("%d words, %d distinct\n", words, len(counts))
}

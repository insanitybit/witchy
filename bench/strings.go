package main

import "fmt"

// Deliberately the same naive accumulate form as the witchy program (not
// strings.Builder) — the comparison is what each compiler does with it.
func main() {
	s := ""
	for i := 0; i < 20000; i++ {
		s += "ab"
	}
	fmt.Println(len(s))
}

package main

import ("fmt"; "strings")

func main() {
	var b strings.Builder
	b.Grow(10000000)
	for i := 0; i < 1000000; i++ { b.WriteString("abcdefghi"); b.WriteByte(10) }
	s := b.String()
	check := uint64(0)
	for i := 0; i < len(s); i++ { check = (check*31 + uint64(s[i])) % 1000000007 }
	fmt.Println(len(s)); fmt.Println(check)
}

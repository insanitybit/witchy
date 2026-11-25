package main

import ("fmt"; "sort")

func main() {
	n := 1000000
	v := make([]uint64, n)
	seed := uint64(42)
	for i := 0; i < n; i++ { seed = seed * 48271 % 2147483647; v[i] = seed % 1000000000 }
	sort.Slice(v, func(a, b int) bool { return v[a] < v[b] })
	check := uint64(0)
	for _, x := range v { check = (check*31 + x) % 1000000007 }
	fmt.Println(check); fmt.Println(v[0]); fmt.Println(v[n-1])
}

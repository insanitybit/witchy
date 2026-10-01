package main

import "fmt"

// Matching observable workloads for dict_table_matrix.witchy. Timings are
// measured by the caller so compile and startup costs can be reported apart
// from steady-state throughput.
func main() {
	hits := make(map[int]int, 1024)
	for i := 0; i < 100000; i++ {
		key := i % 1000
		hits[key] = hits[key] + 1
	}
	fmt.Printf("hit=%d:%d\n", len(hits), hits[0])

	growth := make(map[int]int)
	for i := 0; i < 5000; i++ {
		growth[i] = i + 1
	}
	fmt.Printf("growth=%d:%d\n", len(growth), growth[4999])

	strings := make(map[string]int)
	for i := 0; i < 10000; i++ {
		key := fmt.Sprintf("key-%d", i%100)
		strings[key] = strings[key] + 1
	}
	fmt.Printf("string=%d:%d\n", len(strings), strings["key-0"])

	snapshot := make(map[string]int, len(strings))
	for key, value := range strings {
		snapshot[key] = value
	}
	strings["key-0"] = 999
	fmt.Printf("snapshot=%d:%d\n", snapshot["key-0"], strings["key-0"])
}

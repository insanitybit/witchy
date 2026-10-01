package main

import (
	"fmt"
	"sort"
	"time"
)

func hits() string {
	d := make(map[int]int, 1024)
	for i := 0; i < 100000; i++ {
		key := i % 1000
		d[key] = d[key] + 1
	}
	return fmt.Sprintf("%d:%d", len(d), d[0])
}

func growth() string {
	d := make(map[int]int)
	for i := 0; i < 5000; i++ {
		d[i] = i + 1
	}
	return fmt.Sprintf("%d:%d", len(d), d[4999])
}

func growth20k() string {
	d := make(map[int]int)
	for i := 0; i < 20000; i++ {
		d[i] = i + 1
	}
	return fmt.Sprintf("%d:%d", len(d), d[19999])
}

func strings() string {
	d := make(map[string]int)
	for i := 0; i < 10000; i++ {
		key := fmt.Sprintf("key-%d", i%100)
		d[key] = d[key] + 1
	}
	return fmt.Sprintf("%d:%d", len(d), d["key-0"])
}

func main() {
	cases := []struct {
		name string
		run  func() string
		want string
	}{
		{"integer_hits_100k", hits, "1000:100"},
		{"integer_growth_5k", growth, "5000:5000"},
		{"integer_growth_20k", growth20k, "20000:20000"},
		{"string_updates_10k", strings, "100:100"},
	}
	for _, c := range cases {
		samples := make([]float64, 0, 20)
		for i := 0; i < 23; i++ {
			start := time.Now()
			got := c.run()
			elapsed := float64(time.Since(start).Nanoseconds()) / 1e6
			if got != c.want {
				panic(got)
			}
			if i >= 3 {
				samples = append(samples, elapsed)
			}
		}
		sort.Float64s(samples)
		fmt.Printf("%s run_median_ms=%.3f run_min_ms=%.3f run_max_ms=%.3f\n", c.name,
			(samples[9]+samples[10])/2, samples[0], samples[19])
	}
}

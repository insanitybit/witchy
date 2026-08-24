package main

import (
	"fmt"
	"math"
	"time"
)

func A(i, j int) float64 {
	return 1.0 / float64(((i+j)*(i+j+1))/2+i+1)
}

func multiplyAv(v, Atv []float64) {
	n := len(v)
	for i := 0; i < n; i++ {
		sum := 0.0
		for j := 0; j < n; j++ {
			sum += A(i, j) * v[j]
		}
		Atv[i] = sum
	}
}

func multiplyAtv(v, Atv []float64) {
	n := len(v)
	for i := 0; i < n; i++ {
		sum := 0.0
		for j := 0; j < n; j++ {
			sum += A(j, i) * v[j]
		}
		Atv[i] = sum
	}
}

func multiplyAtAv(v, u, AtAv []float64) {
	multiplyAv(v, u)
	multiplyAtv(u, AtAv)
}

func main() {
	n := 1000
	t0 := time.Now()
	
	u := make([]float64, n)
	v := make([]float64, n)
	tmp := make([]float64, n)
	
	for i := 0; i < n; i++ {
		u[i] = 1.0
	}
	
	for i := 0; i < 10; i++ {
		multiplyAtAv(u, tmp, v)
		multiplyAtAv(v, tmp, u)
	}
	
	vBv := 0.0
	vv := 0.0
	for i := 0; i < n; i++ {
		vBv += u[i] * v[i]
		vv += v[i] * v[i]
	}
	
	res := math.Sqrt(vBv / vv)
	t1 := time.Now()
	
	fmt.Printf("%.9f\n", res)
	fmt.Printf("bench_ns=%d\n", t1.Sub(t0).Nanoseconds())
}

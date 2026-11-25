package main

import ("fmt"; "math")

func a(i, j int) float64 { s := i + j; return 1.0 / float64(s*(s+1)/2+i+1) }
func times(v, out []float64) { for i := range out { s := 0.0; for j := range v { s += a(i, j) * v[j] }; out[i] = s } }
func timesT(v, out []float64) { for i := range out { s := 0.0; for j := range v { s += a(j, i) * v[j] }; out[i] = s } }
func both(v, out, t []float64) { times(v, t); timesT(t, out) }
func main() {
	n := 1000
	u, v, t := make([]float64, n), make([]float64, n), make([]float64, n)
	for i := range u { u[i] = 1 }
	for k := 0; k < 10; k++ { both(u, v, t); both(v, u, t) }
	vbv, vv := 0.0, 0.0
	for i := range u { vbv += u[i] * v[i]; vv += v[i] * v[i] }
	fmt.Printf("%.9f\n", math.Sqrt(vbv/vv))
}

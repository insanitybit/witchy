package main

import (
	"bufio"
	"fmt"
	"os"
)

func main() {
	n := 2000
	size := float64(n)
	out := bufio.NewWriter(os.Stdout)
	fmt.Fprintf(out, "P4\n%d %d\n", n, n)
	for y := 0; y < n; y++ {
		ci := 2.0*float64(y)/size - 1.0
		b, bit := uint64(0), 0
		for x := 0; x < n; x++ {
			cr := 2.0*float64(x)/size - 1.5
			zr, zi, tr, ti := 0.0, 0.0, 0.0, 0.0
			for i := 0; i < 50 && tr+ti <= 4.0; i++ {
				zi = 2.0*zr*zi + ci
				zr = tr - ti + cr
				tr = zr * zr
				ti = zi * zi
			}
			b *= 2
			if tr+ti <= 4.0 {
				b++
			}
			bit++
			if bit == 8 || x == n-1 {
				for bit < 8 {
					b *= 2
					bit++
				}
				out.WriteByte(byte(b))
				b, bit = 0, 0
			}
		}
	}
	if err := out.Flush(); err != nil {
		panic(err)
	}
}

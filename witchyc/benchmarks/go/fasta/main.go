package main

import (
	"bufio"
	"os"
	"strconv"
)

const alu = "GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGGGAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGACCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAATACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCAGCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGGAGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCCAGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA"

var seed uint64 = 42

func rnd() float64 { seed = (seed*3877 + 29573) % 139968; return float64(seed) / 139968.0 }
func main() {
	w := bufio.NewWriter(os.Stdout)
	defer w.Flush()
	n := 250000
	if len(os.Args) > 1 {
		var err error
		n, err = strconv.Atoi(os.Args[1])
		if err != nil || n < 1 {
			panic("expected a positive input size")
		}
	}
	w.WriteString(">ONE Homo sapiens alu\n")
	at, left := 0, 2*n
	for left > 0 {
		l := 60
		if left < 60 {
			l = left
		}
		for j := 0; j < l; j++ {
			w.WriteByte(alu[(at+j)%len(alu)])
		}
		w.WriteByte('\n')
		at = (at + l) % len(alu)
		left -= l
	}
	draw := func(letters string, probs []float64, total int) {
		cum := make([]float64, len(probs))
		s := 0.0
		for i, p := range probs {
			s += p
			cum[i] = s
		}
		for total > 0 {
			l := 60
			if total < 60 {
				l = total
			}
			for j := 0; j < l; j++ {
				r := rnd()
				pick := len(cum) - 1
				for k := range cum {
					if r < cum[k] {
						pick = k
						break
					}
				}
				w.WriteByte(letters[pick])
			}
			w.WriteByte('\n')
			total -= l
		}
	}
	w.WriteString(">TWO IUB ambiguity codes\n")
	draw("acgtBDHKMNRSVWY", []float64{0.27, 0.12, 0.12, 0.27, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02}, 3*n)
	w.WriteString(">THREE Homo sapiens frequency\n")
	draw("acgt", []float64{0.3029549426680, 0.1979883004921, 0.1975473066391, 0.3015094502008}, 5*n)
}

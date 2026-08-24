package main

import (
	"fmt"
	"time"
)

var alu = []byte("GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGGGAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGACCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAATACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCAGCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGGAGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCCAGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA")

var complement = [256]byte{}

func init() {
	for i := 0; i < 256; i++ {
		complement[i] = byte(i)
	}
	complement['A'] = 'T'
	complement['a'] = 'T'
	complement['C'] = 'G'
	complement['c'] = 'G'
	complement['G'] = 'C'
	complement['g'] = 'C'
	complement['T'] = 'A'
	complement['t'] = 'A'
	complement['U'] = 'A'
	complement['u'] = 'A'
	complement['M'] = 'K'
	complement['m'] = 'K'
	complement['R'] = 'Y'
	complement['r'] = 'Y'
	complement['W'] = 'W'
	complement['w'] = 'W'
	complement['S'] = 'S'
	complement['s'] = 'S'
	complement['Y'] = 'R'
	complement['y'] = 'R'
	complement['K'] = 'M'
	complement['k'] = 'M'
	complement['V'] = 'B'
	complement['v'] = 'B'
	complement['H'] = 'D'
	complement['h'] = 'D'
	complement['D'] = 'H'
	complement['d'] = 'H'
	complement['B'] = 'V'
	complement['b'] = 'V'
	complement['N'] = 'N'
	complement['n'] = 'N'
}

func main() {
	total := 3000000
	seq := make([]byte, total)
	for i := 0; i < total; i++ {
		seq[i] = alu[i%len(alu)]
	}
	
	t0 := time.Now()

	// Do it 15 times to consume ~0.4s CPU time
	for iter := 0; iter < 15; iter++ {
		for i, j := 0, total-1; i <= j; i, j = i+1, j-1 {
			ci := seq[i]
			cj := seq[j]
			seq[i] = complement[cj]
			seq[j] = complement[ci]
		}
	}

	t1 := time.Now()

	fmt.Println(">ONE Homo sapiens alu")
	for i := 0; i < total; i += 60 {
		end := i + 60
		if end > total {
			end = total
		}
		fmt.Println(string(seq[i:end]))
	}

	fmt.Printf("bench_ns=%d\n", t1.Sub(t0).Nanoseconds())
}

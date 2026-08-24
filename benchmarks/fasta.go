package main

import (
	"bufio"
	"fmt"
	"os"
	"time"
)

var last = 42

const (
	A = 3877
	C = 29573
	M = 139968
)

func randf(max float64) float64 {
	last = (last*A + C) % M
	return max * float64(last) / float64(M)
}

const ALU = "GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGG" +
	"GAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGA" +
	"CCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAAT" +
	"ACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCA" +
	"GCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGG" +
	"AGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCC" +
	"AGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA"

type AA struct {
	p float64
	c byte
}

var IUB = []AA{
	{0.27, 'a'}, {0.12, 'c'}, {0.12, 'g'}, {0.27, 't'},
	{0.02, 'B'}, {0.02, 'D'}, {0.02, 'H'}, {0.02, 'K'},
	{0.02, 'M'}, {0.02, 'N'}, {0.02, 'R'}, {0.02, 'S'},
	{0.02, 'V'}, {0.02, 'W'}, {0.02, 'Y'},
}

var HomoSap = []AA{
	{0.3029549426680, 'a'},
	{0.1979883004921, 'c'},
	{0.1975473066391, 'g'},
	{0.3015094502008, 't'},
}

func makeCumulative(table []AA) {
	cp := 0.0
	for i := range table {
		cp += table[i].p
		table[i].p = cp
	}
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}

func fastaRepeat(out *bufio.Writer, n int, seq string) {
	seqi := 0
	buf := make([]byte, 0, 60+1)
	for n > 0 {
		lenOut := min(n, 60)
		buf = buf[:0]
		if seqi+lenOut < len(seq) {
			buf = append(buf, seq[seqi:seqi+lenOut]...)
			seqi += lenOut
		} else {
			s := seq[seqi:]
			seqi = lenOut - len(s)
			buf = append(buf, s...)
			buf = append(buf, seq[:seqi]...)
		}
		buf = append(buf, '\n')
		out.Write(buf)
		n -= lenOut
	}
}

func fastaRandom(out *bufio.Writer, n int, table []AA) {
	buf := make([]byte, 60+1)
	for n > 0 {
		lenOut := min(n, 60)
		buf = buf[:lenOut]
		for i := 0; i < lenOut; i++ {
			r := randf(1.0)
			for j := range table {
				if r < table[j].p {
					buf[i] = table[j].c
					break
				}
			}
		}
		buf = append(buf, '\n')
		out.Write(buf)
		n -= lenOut
	}
}

func main() {
	t0 := time.Now()

	makeCumulative(IUB)
	makeCumulative(HomoSap)

	n := 2500000
	out := bufio.NewWriter(os.Stdout)

	out.WriteString(">ONE Homo sapiens alu\n")
	fastaRepeat(out, 2*n, ALU)

	out.WriteString(">TWO IUB ambiguity codes\n")
	fastaRandom(out, 3*n, IUB)

	out.WriteString(">THREE Homo sapiens frequency\n")
	fastaRandom(out, 5*n, HomoSap)
	
	out.Flush()

	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

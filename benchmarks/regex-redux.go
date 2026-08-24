package main

import (
	"fmt"
	"regexp"
	"strings"
	"time"
)

type subst struct {
	re                *regexp.Regexp
	replacementString []byte
}

type variant struct {
	re     *regexp.Regexp
	result int
}

func main() {
	t0 := time.Now()
	
	seq := "agggtaaa cgtgggtaaa aactggtaaa agactgtaaa aggacttaaa agggcgaaa agggtcaa agggtaca agggtaac aatggtaaa agagtaaa aggataaa agggcaaa tHaNt aND caN HaD WaS aNt BY <header> |word| "
	
	// build string
	var sb strings.Builder
	for i := 0; i < 2500; i++ {
		sb.WriteString(">Sequence_")
		sb.WriteString(fmt.Sprintf("%d\n", i))
		sb.WriteString(seq)
		sb.WriteString("\n")
	}
	bytes := []byte(sb.String())
	originalLen := len(bytes)

	cleanRE := &subst{regexp.MustCompile("(>[^\n]+)?\n"), []byte("")}
	bytes = cleanRE.re.ReplaceAllLiteral(bytes, cleanRE.replacementString)
	cleanedLen := len(bytes)

	variants := []*variant{
		{re: regexp.MustCompile("agggtaaa|tttaccct")},
		{re: regexp.MustCompile("[cgt]gggtaaa|tttaccc[acg]")},
		{re: regexp.MustCompile("a[act]ggtaaa|tttacc[agt]t")},
		{re: regexp.MustCompile("ag[act]gtaaa|tttac[agt]ct")},
		{re: regexp.MustCompile("agg[act]taaa|ttta[agt]cct")},
		{re: regexp.MustCompile("aggg[acg]aaa|ttt[cgt]ccct")},
		{re: regexp.MustCompile("agggt[cgt]aa|tt[acg]accct")},
		{re: regexp.MustCompile("agggta[cgt]a|t[acg]taccct")},
		{re: regexp.MustCompile("agggtaa[cgt]|[acg]ttaccct")},
	}
	for _, v := range variants {
		v.result = len(v.re.FindAll(bytes, -1))
		fmt.Printf("%s %d\n", v.re.String(), v.result)
	}

	substitutions := []*subst{
		{regexp.MustCompile("tHa[Nt]"), []byte("<4>")},
		{regexp.MustCompile("aND|caN|Ha[DS]|WaS"), []byte("<3>")},
		{regexp.MustCompile("a[NSt]|BY"), []byte("<2>")},
		{regexp.MustCompile("<[^>]*>"), []byte("|")},
		{regexp.MustCompile(`\|[^|][^|]*\|`), []byte("-")},
	}
	for _, sub := range substitutions {
		bytes = sub.re.ReplaceAll(bytes, sub.replacementString)
	}
	fmt.Printf("\n%d\n%d\n%d\n", originalLen, cleanedLen, len(bytes))
	fmt.Printf("bench_ns=%d\n", time.Since(t0).Nanoseconds())
}

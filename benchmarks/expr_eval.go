package main

import "fmt"

// Go has no sum types, so an AST is a tagged struct with pointer children and a
// kind switch — the idiomatic equivalent of witchy's `type Expr` + `match`.
type Expr struct {
	kind byte // 0 = Num, 1 = Add, 2 = Mul
	n    int
	a, b *Expr
}

func num(n int) *Expr      { return &Expr{kind: 0, n: n} }
func add(a, b *Expr) *Expr { return &Expr{kind: 1, a: a, b: b} }
func mul(a, b *Expr) *Expr { return &Expr{kind: 2, a: a, b: b} }

func eval(e *Expr) int {
	switch e.kind {
	case 0:
		return e.n
	case 1:
		return eval(e.a) + eval(e.b)
	default:
		return eval(e.a) * eval(e.b)
	}
}

func build(depth int) *Expr {
	if depth <= 0 {
		return num(1)
	}
	return add(mul(build(depth-1), num(2)), build(depth-1))
}

func main() {
	total := 0
	for i := 0; i < 10; i++ {
		total += eval(build(16))
	}
	fmt.Println(total)
}

use std::time::Instant;

enum Expr {
    Num(i32),
    Add(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
}

fn num(n: i32) -> Box<Expr> { Box::new(Expr::Num(n)) }
fn add(a: Box<Expr>, b: Box<Expr>) -> Box<Expr> { Box::new(Expr::Add(a, b)) }
fn mul(a: Box<Expr>, b: Box<Expr>) -> Box<Expr> { Box::new(Expr::Mul(a, b)) }

fn eval(e: &Expr) -> i32 {
    match e {
        Expr::Num(n) => *n,
        Expr::Add(a, b) => eval(a) + eval(b),
        Expr::Mul(a, b) => eval(a) * eval(b),
    }
}

fn build(depth: i32) -> Box<Expr> {
    if depth <= 0 {
        return num(1);
    }
    add(mul(build(depth - 1), num(2)), build(depth - 1))
}

fn main() {
    let t0 = Instant::now();
    let mut total = 0;
    for _ in 0..10 {
        total += eval(&build(16));
    }
    println!("{}", total);
    println!("bench_ns={}", t0.elapsed().as_nanos());
}

t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

class Num
  attr_reader :n
  def initialize(n); @n = n; end
  def eval; @n; end
end

class Add
  def initialize(a, b); @a = a; @b = b; end
  def eval; @a.eval + @b.eval; end
end

class Mul
  def initialize(a, b); @a = a; @b = b; end
  def eval; @a.eval * @b.eval; end
end

def build(depth)
  if depth <= 0
    Num.new(1)
  else
    Add.new(Mul.new(build(depth - 1), Num.new(2)), build(depth - 1))
  end
end

total = 0
10.times do
  total += build(16).eval
end
puts total
puts "bench_ns=#{Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond) - t0}"

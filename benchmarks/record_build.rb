t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

class Stack
  attr_accessor :items, :total
  def initialize
    @items = []
    @total = 0
  end
end

def build(n)
  s = Stack.new
  i = 0
  while i < n
    s.items << i
    s.total += i
    i += 1
  end
  s
end

s = build(500000)
puts (s.total + s.items.length)
puts "bench_ns=#{Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond) - t0}"

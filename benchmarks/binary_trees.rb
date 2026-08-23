class Node
  attr_reader :left, :right
  def initialize(left, right)
    @left = left
    @right = right
  end
end

def build(depth)
  if depth == 0
    Node.new(nil, nil)
  else
    Node.new(build(depth - 1), build(depth - 1))
  end
end

def check(node)
  if node.nil?
    0
  else
    1 + check(node.left) + check(node.right)
  end
end

t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
total = 0
50.times do
  t = build(16)
  total += check(t)
end
t1 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

puts total
puts "bench_ns=#{t1 - t0}"

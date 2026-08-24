require 'thread'

t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
q = SizedQueue.new(64)
n = 64000

t = Thread.new do
  i = 0
  while i < n
    q.push(i)
    i += 1
  end
  q.close
end

sum = 0
while (v = q.pop)
  sum += v
end
t.join

t1 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
puts sum
puts "bench_ns=#{t1 - t0}"

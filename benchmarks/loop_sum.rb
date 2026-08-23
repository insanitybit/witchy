t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
s = 0
i = 0
while i < 100000000
  s += i
  i += 1
end
t1 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

puts s
puts "bench_ns=#{t1 - t0}"

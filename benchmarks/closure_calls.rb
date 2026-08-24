t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
f = ->(x) { x % 7 }
total = 0
i = 0
while i < 5000000
    total += f.call(i)
    i += 1
end
puts total
puts "bench_ns=#{Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond) - t0}"

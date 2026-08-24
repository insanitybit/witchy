t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
acc = []
i = 0
while i < 3000000
    acc << i
    i += 1
end
total = 0
acc.each do |x|
    total += x
end
puts total
puts "bench_ns=#{Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond) - t0}"

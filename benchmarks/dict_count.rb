t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
d = Hash.new(0)
i = 0
while i < 3000000
    k = i % 1000
    d[k] += 1
    i += 1
end
total = 0
d.each_value do |v|
    total += v
end
puts total
puts "bench_ns=#{Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond) - t0}"

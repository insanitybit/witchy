t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
d = Hash.new(0)
i = 0
while i < 1000000
    w = "word#{i % 1000}"
    d[w] += 1
    i += 1
end
total = 0
d.each_value do |v|
    total += v
end
puts (total + d.size)
puts "bench_ns=#{Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond) - t0}"

t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
xs = Array.new(5000)
i = 0
while i < 5000
    xs[i] = i
    i += 1
end
total = 0
rep = 0
while rep < 2000
    i = 0
    len = xs.length
    while i < len
        total += xs[i]
        i += 1
    end
    rep += 1
end
puts total
puts "bench_ns=#{Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond) - t0}"

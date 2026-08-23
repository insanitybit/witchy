def steps(start)
  n = start
  c = 0
  while n > 1
    if n % 2 == 0
      n = n / 2
    else
      n = 3 * n + 1
    end
    c += 1
  end
  c
end

t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
total = 0
i = 1
while i < 1000000
  total += steps(i)
  i += 1
end
t1 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

puts total
puts "bench_ns=#{t1 - t0}"

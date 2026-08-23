def fib(n)
  if n < 2
    n
  else
    fib(n - 1) + fib(n - 2)
  end
end

t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
r = fib(35)
t1 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

puts r
puts "bench_ns=#{t1 - t0}"

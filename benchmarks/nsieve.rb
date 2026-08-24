t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

def nsieve(n)
  flags = Array.new(n, true)
  count = 0
  i = 2
  while i < n
    if flags[i]
      count += 1
      j = i + i
      while j < n
        flags[j] = false
        j += i
      end
    end
    i += 1
  end
  count
end

total = 0
total += nsieve(800000)
total += nsieve(400000)
total += nsieve(200000)
puts total
puts "bench_ns=#{Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond) - t0}"

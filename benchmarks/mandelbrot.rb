t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
total = 0
y = -1.5
while y < 1.5
  x = -2.0
  while x < 1.0
    zr = 0.0
    zi = 0.0
    i = 0
    while i < 255 && zr * zr + zi * zi <= 4.0
      nzr = zr * zr - zi * zi + x
      nzi = 2.0 * zr * zi + y
      zr = nzr
      zi = nzi
      i += 1
    end
    total += i
    x += 0.005
  end
  y += 0.005
end
t1 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

puts total
puts "bench_ns=#{t1 - t0}"

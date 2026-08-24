t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

n = 200000
k = 10
cs = String.new(capacity: n)
seed = 42
chars = "ACGT"

i = 0
while i < n
  seed = (seed * 1103515245 + 12345) % 2147483648
  cs << chars[(seed / 65536) % 4]
  i += 1
end

counts = Hash.new(0)
j = 0
limit = n - k
while j <= limit
  counts[cs[j, k]] += 1
  j += 1
end

puts counts.size
puts "bench_ns=#{Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond) - t0}"

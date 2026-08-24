PER_SOURCE = 2048

def expected_checksum
  checksum = 0
  PER_SOURCE.times do |i|
    checksum = (checksum * 257 + i * 2 + 1) % 1000000007
  end
  PER_SOURCE.times do |i|
    checksum = (checksum * 257 + i * 2 + 3) % 1000000007
  end
  checksum
end

a = []
b = []
PER_SOURCE.times { |i| a << (i * 2) }
PER_SOURCE.times { |i| b << (1 + i * 2) }

seen_a = 0
seen_b = 0
checksum = 0
order_ok = true

t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
while seen_a + seen_b < PER_SOURCE * 2
  if !a.empty?
    val = a.shift
    source = 0
  elsif !b.empty?
    val = b.shift
    source = 1
  else
    break
  end

  if source == 0
    selected_ok = (seen_b == 0 && val == seen_a * 2)
    seen_a += 1
    checksum = (checksum * 257 + val + 1) % 1000000007
  else
    selected_ok = (seen_a == 2048 && val == seen_b * 2 + 1)
    seen_b += 1
    checksum = (checksum * 257 + val + 2) % 1000000007
  end
  order_ok &&= selected_ok
end
t1 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

expected = expected_checksum
if order_ok && seen_a == 2048 && seen_b == 2048 && checksum == expected
  puts "count=#{seen_a + seen_b} checksum=#{checksum} order=ok"
else
  puts "count=#{seen_a + seen_b} checksum=#{checksum} expected=#{expected} order=bad"
end
puts "bench_ns=#{t1 - t0}"

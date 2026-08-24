t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

def fannkuch(n)
  perm1 = Array.new(n) { |i| i }
  count = Array.new(n, 0)
  perm = Array.new(n, 0)
  max_flips = 0
  checksum = 0
  perm_count = 0
  r = n

  loop do
    while r != 1
      count[r - 1] = r
      r -= 1
    end

    if perm1[0] != 0
      perm = perm1.dup
      flips = 0
      k = perm[0]
      while k != 0
        lo = 0
        hi = k
        while lo < hi
          perm[lo], perm[hi] = perm[hi], perm[lo]
          lo += 1
          hi -= 1
        end
        flips += 1
        k = perm[0]
      end
      max_flips = flips if flips > max_flips
      if perm_count % 2 == 0
        checksum += flips
      else
        checksum -= flips
      end
    end

    advanced = false
    until advanced
      if r == n
        puts (checksum * 1000 + max_flips)
        return max_flips
      end
      perm0 = perm1[0]
      i = 0
      while i < r
        perm1[i] = perm1[i + 1]
        i += 1
      end
      perm1[r] = perm0
      count[r] -= 1
      if count[r] > 0
        advanced = true
      else
        r += 1
      end
    end
    perm_count += 1
  end
end

fannkuch(10)
puts "bench_ns=#{Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond) - t0}"

def eval_A(i, j)
  1.0 / (((i + j) * (i + j + 1)) / 2 + i + 1)
end

def multiplyAv(v, atv)
  n = v.length
  n.times do |i|
    sum = 0.0
    n.times do |j|
      sum += eval_A(i, j) * v[j]
    end
    atv[i] = sum
  end
end

def multiplyAtv(v, atv)
  n = v.length
  n.times do |i|
    sum = 0.0
    n.times do |j|
      sum += eval_A(j, i) * v[j]
    end
    atv[i] = sum
  end
end

def multiplyAtAv(v, tmp, atAv)
  multiplyAv(v, tmp)
  multiplyAtv(tmp, atAv)
end

n = 1000
u = Array.new(n, 1.0)
v = Array.new(n, 0.0)
tmp = Array.new(n, 0.0)

t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

10.times do
  multiplyAtAv(u, tmp, v)
  multiplyAtAv(v, tmp, u)
end

vBv = 0.0
vv = 0.0
n.times do |i|
  vBv += u[i] * v[i]
  vv += v[i] * v[i]
end

res = Math.sqrt(vBv / vv)
t1 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

puts format("%.9f", res)
puts "bench_ns=#{t1 - t0}"

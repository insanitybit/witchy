alu = "GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGGGAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGACCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAATACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCAGCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGGAGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCCAGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA".bytes

total = 3000000

seq = Array.new(total, 0)
alu_len = alu.length
for i in 0...total do
    seq[i] = alu[i % alu_len]
end

comp = Array.new(256, 0)
for i in 0...256 do
    comp[i] = i
end
comp['A'.ord] = 'T'.ord
comp['a'.ord] = 'T'.ord
comp['C'.ord] = 'G'.ord
comp['c'.ord] = 'G'.ord
comp['G'.ord] = 'C'.ord
comp['g'.ord] = 'C'.ord
comp['T'.ord] = 'A'.ord
comp['t'.ord] = 'A'.ord

t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

for iter in 0...15 do
    i = 0
    j = total - 1
    while i <= j do
        ci = seq[i]
        cj = seq[j]
        seq[i] = comp[cj]
        seq[j] = comp[ci]
        i += 1
        j -= 1
    end
end

t1 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

puts ">ONE Homo sapiens alu"
k = 0
while k < total do
    end_idx = k + 60
    end_idx = total if end_idx > total
    puts seq[k...end_idx].map(&:chr).join
    k += 60
end

puts "bench_ns=#{t1 - t0}"

t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

$last = 42
A = 3877
C = 29573
M = 139968

def randf(max)
  $last = ($last * A + C) % M
  max * $last.to_f / M.to_f
end

ALU = "GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGG" \
      "GAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGA" \
      "CCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAAT" \
      "ACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCA" \
      "GCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGG" \
      "AGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCC" \
      "AGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA"

IUB = [
  [0.27, 'a'], [0.12, 'c'], [0.12, 'g'], [0.27, 't'],
  [0.02, 'B'], [0.02, 'D'], [0.02, 'H'], [0.02, 'K'],
  [0.02, 'M'], [0.02, 'N'], [0.02, 'R'], [0.02, 'S'],
  [0.02, 'V'], [0.02, 'W'], [0.02, 'Y']
]

HomoSap = [
  [0.3029549426680, 'a'],
  [0.1979883004921, 'c'],
  [0.1975473066391, 'g'],
  [0.3015094502008, 't']
]

def make_cumulative(table)
  cp = 0.0
  table.each do |t|
    cp += t[0]
    t[0] = cp
  end
end

make_cumulative(IUB)
make_cumulative(HomoSap)

def fasta_repeat(n, seq)
  seqi = 0
  len_out = 60
  while n > 0
    len_out = n if n < len_out
    if seqi + len_out < seq.length
      $stdout.write(seq[seqi, len_out])
      $stdout.write("\n")
      seqi += len_out
    else
      s = seq[seqi..-1]
      seqi = len_out - s.length
      $stdout.write(s)
      $stdout.write(seq[0, seqi])
      $stdout.write("\n")
    end
    n -= len_out
  end
end

def fasta_random(n, table)
  while n > 0
    len_out = n < 60 ? n : 60
    line = ""
    len_out.times do
      r = randf(1.0)
      table.each do |t|
        if r < t[0]
          line << t[1]
          break
        end
      end
    end
    $stdout.write(line)
    $stdout.write("\n")
    n -= len_out
  end
end

n = 2500000

$stdout.write(">ONE Homo sapiens alu\n")
fasta_repeat(2 * n, ALU)

$stdout.write(">TWO IUB ambiguity codes\n")
fasta_random(3 * n, IUB)

$stdout.write(">THREE Homo sapiens frequency\n")
fasta_random(5 * n, HomoSap)

t1 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
$stdout.write("bench_ns=#{t1 - t0}\n")

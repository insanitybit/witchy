t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)

seq = "agggtaaa cgtgggtaaa aactggtaaa agactgtaaa aggacttaaa agggcgaaa agggtcaa agggtaca agggtaac aatggtaaa agagtaaa aggataaa agggcaaa tHaNt aND caN HaD WaS aNt BY <header> |word| "

text = ""
250.times do |i|
  text += ">Sequence_#{i}\n#{seq}\n"
end
originalLen = text.bytesize

text = text.gsub(/(>[^\n]+)?\n/, "")
cleanedLen = text.bytesize

variants = [
  /agggtaaa|tttaccct/,
  /[cgt]gggtaaa|tttaccc[acg]/,
  /a[act]ggtaaa|tttacc[agt]t/,
  /ag[act]gtaaa|tttac[agt]ct/,
  /agg[act]taaa|ttta[agt]cct/,
  /aggg[acg]aaa|ttt[cgt]ccct/,
  /agggt[cgt]aa|tt[acg]accct/,
  /agggta[cgt]a|t[acg]taccct/,
  /agggtaa[cgt]|[acg]ttaccct/
]

variants.each do |re|
  count = text.scan(re).size
  puts "#{re.source} #{count}"
end

substitutions = [
  [/tHa[Nt]/, "<4>"],
  [/aND|caN|Ha[DS]|WaS/, "<3>"],
  [/a[NSt]|BY/, "<2>"],
  [/<[^>]*>/, "|"],
  [/\|[^|][^|]*\|/, "-"]
]

substitutions.each do |re, sub|
  text = text.gsub(re, sub)
end

puts "\n#{originalLen}\n#{cleanedLen}\n#{text.bytesize}"
t1 = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
puts "bench_ns=#{t1 - t0}"

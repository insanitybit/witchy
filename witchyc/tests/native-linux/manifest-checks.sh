#!/bin/sh
# Linux ARM64 manifest checks, run in a local Linux container with no network: build witchyc
# offline (source and ~/.cargo/registry mounted read-only, CARGO_HOME=/cargo with
# /cargo/registry -> /registry, `mbx build --offline --locked -j 1 --bin witchyc` into
# /target), copy this directory to /w, then:
#   docker run --rm --network none -v SRC:/src:ro -v TARGET:/target:ro -v WORK:/w IMAGE sh /w/manifest-checks.sh
# Set WITCHY_RUNTIME_RUST_ARCHIVE to the libwitchy_runtime.a built with that compiler to probe
# the runtime directly. SRC is otherwise optional.
# With AWS-LC (build-aws-lc.sh) mounted at /awslc and witchyc built with WITCHY_NATIVE_AWS_LC_ROOT=/awslc,
# signing is checked too.
# Run under Docker's default seccomp profile: it allows Landlock (ABI 4 on the 6.8 kernel used),
# openat2, close_range, pidfds and the child subreaper, which is all the launch uses.
W=/target/debug/witchyc
fail=0
check() { if [ "$1" = "$2" ]; then echo "ok   $3"; else echo "FAIL $3: got [$1] want [$2]"; fail=1; fi; }
build() { $W build "$1.wy" -o "$1" --target=linux-arm64 --manifest="$1.grants.toml" || { echo "FAIL build $1"; fail=1; }; }
cd /w
rm -rf scratch && mkdir -p scratch && cd scratch && cp ../*.wy ../*.toml . && mkdir -p bin && cp ../greet.sh bin/greet && cp ../escape.sh bin/escape && cp ../hang.sh bin/hang && cp ../nnp.sh bin/nnp && cp ../killer.sh bin/killer && cp ../answer.sh bin/answer && cp ../nap.sh bin/nap && cp ../where.sh bin/where && cp ../showenv.sh bin/showenv && chmod +x bin/*
# The exec programs are planned at /w/bin.
rm -rf /w/bin && cp -R bin /w/bin

# Plans of Console<Write> and Allocator; development flags and tampered plans refused.
build hello
check "$(./hello)" "42" "manifest hello prints with no flags"
out=$(./hello --output 2>&1); check "$?" "2" "manifest hello refuses a development flag"
echo "$out" | grep -q "not a declared argument" && echo "ok   refusal names the argument" || { echo "FAIL refusal text: $out"; fail=1; }
check "$(./hello 7</etc/hostname)" "42" "an inherited descriptor is closed at launch"
cp hello tampered && printf 'x' | dd of=tampered bs=1 seek=$(grep -abo 'witchy-launch-plan' tampered | head -1 | cut -d: -f1) conv=notrunc 2>/dev/null
./tampered >/dev/null 2>&1; check "$?" "2" "a plan whose header is edited refuses the launch"
out=$(./hello </dev/null 2>&1 | head -1)

# Dir<Read> through the confined broker: the notes-tool milestone, links, .., hard links.
mkdir -p notes && printf 'hi' > notes/a.txt && printf 'o' > outside.txt && ln -s a.txt notes/link.txt
build notes-tool
check "$(./notes-tool --project notes | tr '\n' ' ')" "0 104 " "notes-tool milestone reads its directory"
ln -s notes linked
./notes-tool --project linked >/dev/null 2>&1; check "$?" "2" "a root reached through a link is refused"
build reader
printf 'b' > notes/b.txt; ln notes/b.txt hard.txt
check "$(./reader --project notes | tr '\n' ' ')" "0 2 2 2 " "a file opens; .., a second hard link and a symbolic link are refused"

# Dir<Write>: make, create-only, rename, remove, and nothing outside.
mkdir -p data && printf 'g' > data/gone.txt
build writer
check "$(./writer | tr '\n' ' ')" "0 0 0 0 2 " "a writable directory: mkdir, create, rename, remove; .. refused"
[ -f data/moved.txt ] && [ ! -e data/gone.txt ] && [ ! -e escape.txt ] && echo "ok   the writes landed inside, nothing outside" || { echo "FAIL write effects"; fail=1; }

# Exec through the runner: only the granted program, no environment; a descendant that starts
# its own session and is orphaned at once is still stopped (the runner is its subreaper).
build exec
check "$(./exec | tr '\n' ' ')" "hello, w (1) env=unset 0 " "Exec runs the granted program with nothing else"
# The Exec builder's runtime: input, standard error, and settings that only narrow.
build builder
check "$(./builder | tr '\n' ' ')" "read [in] args x warned 1 3 7 7 2 3 6 " "Exec input, standard error, and narrowing settings (wider refused)"
# Command.env and Command.cwd: a declared variable only, and a granted directory as the program's
# working directory (its opened descriptor passed to the runner, never a path).
mkdir -p ecdata && echo top > ecdata/marker
build envcwd
check "$(./envcwd | tr '\n' ' ')" "LANG=C TZ=unset HOME=unset USER=unset 1 top " "Exec variables only by declared name, and a granted directory to run in"
sed 's/env = \["LANG", "TZ"\]/env = ["LANG", "LD_PRELOAD"]/' envcwd.grants.toml > ldpre.grants.toml && cp envcwd.wy ldpre.wy
if $W build ldpre.wy -o ldpre --target=linux-arm64 --manifest=ldpre.grants.toml >/dev/null 2>&1; then echo "FAIL a plan declaring LD_PRELOAD was built"; fail=1; else echo "ok   a variable that loads code (LD_PRELOAD) cannot be declared"; fi
build nnp
check "$(./nnp)" "CapEff:0000000000000000 CapBnd:0000000000000000 CapAmb:0000000000000000 NoNewPrivs:1 " "a program run through Exec (launched as root) holds no_new_privs and no capability"
build escape
rm -f /w/escaped; ./escape >/dev/null; sleep 2
[ ! -e /w/escaped ] && echo "ok   an orphaned setsid descendant is stopped with its program" || { echo "FAIL an escaped descendant outlived its run"; fail=1; }

# A program that leaves 1500 descendants in sessions of their own and one that keeps forking:
# all are stopped (or the run is failed), and nothing is left running.
count() { ls /proc | grep -c '^[0-9]*$'; }
cc -O2 -o /w/bin/bomb /w/bomb.c
build bomb
before=$(count)
./bomb >/dev/null 2>&1
sleep 1
after=$(count)
[ "$after" -le "$before" ] && echo "ok   a forking descendant population is stopped completely ($before -> $after processes)" || { echo "FAIL processes left: $before -> $after"; fail=1; }

# The guest's deadline expires while the program runs with orphaned setsid descendants: all of
# it goes, and a process outside the run in the launcher's own group is not signalled.
build hang
rm -f /w/hung
sleep 60 &
bystander=$!
before=$(count)
started=$(date +%s)
./hang >/dev/null 2>&1; code=$?
took=$(( $(date +%s) - started ))
sleep 1
after=$(count)
[ $code -ne 0 ] && [ $took -le 5 ] && [ "$after" -le "$before" ] && kill -0 $bystander 2>/dev/null \
  && echo "ok   at the deadline the program and its orphans are gone, nothing outside is signalled ($before -> $after, ${took}s)" \
  || { echo "FAIL deadline: code $code, ${took}s, $before -> $after, bystander $(kill -0 $bystander 2>&1 && echo alive)"; fail=1; }
kill $bystander 2>/dev/null

# A program that kills the runner (its parent) after leaving orphaned setsid descendants: the
# warden, a subreaper between the broker and the runner, stops them.
build killer
rm -f /w/killed
before=$(count)
./killer >/dev/null 2>&1
sleep 4
after=$(count)
[ ! -e /w/killed ] && [ "$after" -le "$before" ] && echo "ok   a program that kills the runner leaves nothing: the warden stops its orphans ($before -> $after)" || { echo "FAIL killed runner: $before -> $after, $(cat /w/killed 2>/dev/null)"; fail=1; }

# In a socket-entry server, a program that kills the runner and the warden after leaving
# orphans: the supervisor, a subreaper too, stops them before serving on.
cp /w/chainkill.sh /w/bin/chainkill && chmod +x /w/bin/chainkill
build chainkill
rm -f /w/chainkilled
before=$(count)
./chainkill >/dev/null 2>&1 &
cpid=$!
sleep 1
perl -MIO::Socket::INET -e '$s = IO::Socket::INET->new(PeerAddr => "127.0.0.1:47041") or exit; print $s "a"; local $/; <$s>' >/dev/null 2>&1
wait $cpid
sleep 4
after=$(count)
[ ! -e /w/chainkilled ] && [ "$after" -le "$before" ] && echo "ok   a server's program that kills the runner and warden leaves nothing: the supervisor stops its orphans ($before -> $after)" || { echo "FAIL chain kill: $before -> $after, $(cat /w/chainkilled 2>/dev/null)"; fail=1; }

# RLIMIT_NPROC: the plan's exec-processes beyond the user's own count, for a user that is not
# root (the kernel exempts root).
cc -O2 -o /w/bin/forks /w/forks.c
build forks
if id witchyuser >/dev/null 2>&1 || useradd -M witchyuser 2>/dev/null; then
  chmod -R a+rwX /w
  started=$(su witchyuser -s /bin/sh -c ./forks 2>/dev/null)
  [ -n "$started" ] && [ "$started" -le 20 ] && [ "$started" -ge 1 ] && echo "ok   exec-processes = 20 bounds what a program starts ($started)" || { echo "FAIL exec-processes: started [$started]"; fail=1; }
else
  echo "FAIL could not add a user for the RLIMIT_NPROC check"; fail=1
fi

# A socket-entry server with a read file, the input, outbound Net, Exec and a writable
# directory, per-connection entries: two connections, each from the start.
printf 'FG' > f.txt && mkdir -p sdata
perl -MIO::Socket::INET -e '$l = IO::Socket::INET->new(LocalAddr => "127.0.0.1:47012", Listen => 5, ReuseAddr => 1) or die; for (1..2) { $c = $l->accept; $c->read($b, 1); print $c chr(ord($b) + 1); close $c }' &
upstream=$!
build server
printf 'IJ' | ./server &
server=$!
sleep 1
replies=""
for name in a b; do
  r=$(perl -MIO::Socket::INET -e '$s = IO::Socket::INET->new(PeerAddr => "127.0.0.1:47011") or die; print $s $ARGV[0]; local $/; print scalar <$s>' $name)
  replies="$replies$r "
done
wait $server; wait $upstream
check "$replies" "FIbh03 FIch03 " "a socket server: file and input from their start, Net, Exec, per-connection entries"

# Signing with the AWS-LC FIPS module, when the compiler was built with it (mounted at /awslc).
if [ -f /awslc/lib/libaws_lc_fips_0_14_4_crypto.a ]; then
  : "${WITCHY_RUNTIME_RUST_ARCHIVE:?set to the Rust runtime archive built with witchyc}"
  printf '464756A985E5DF03739D95DD309C1ED9C5B04254CC294E7E7EB9B9365EE15117' | perl -ne 'print pack("H*", $_)' > seed
  cc -Wl,--gc-sections -DBORINGSSL_PREFIX=aws_lc_fips_0_14_4 -I/awslc/include -O2 -o signer-probe /w/signer-probe.c "$WITCHY_RUNTIME_RUST_ARCHIVE" /awslc/lib/libaws_lc_fips_0_14_4_crypto.a -lpthread
  check "$(./signer-probe 3<seed | tr '\n' ' ')" "fips 1 73615fc5aaa407846b296677b2803e29c3f555db426c69103a3cb45d584730c531ba2d818786b2a9d09b13100e3174f1 sign 0 verify 1 other role 1 " "the FIPS module signs under the guest sandbox: ACVP key, a verified signature, the role's domain only"
  # The language's Signer: the seed from a declared descriptor argument only.
  printf 'fn main(out: Console<Write>, targets: Signer):\n    out.print_int(7)\n    var short: [U8; 1] = [0]\n    out.print_int(targets.raw_public_key(short.slice_mut()))\n' > signer.wy
  printf '[launch]\nversion = 1\n[flags]\nkey-fd = { type = "fd", secret = true }\n[secrets]\ntargets = { use = "sign/witchy-2/targets", from = { from = "arg", name = "key-fd" } }\n' > signer.grants.toml
  build signer
  check "$(./signer --key-fd 3 3<seed | tr '\n' ' ')" "7 2 " "a Signer from a declared descriptor argument"
  ./signer --key-fd 4 3<seed >/dev/null 2>&1; check "$?" "2" "a Signer from a descriptor that is not open is refused"
  ./signer --key-fd 3 --secret-slot=targets:sign/witchy-2/targets:fd:3 3<seed >/dev/null 2>&1; check "$?" "2" "a development secret flag is refused beside the plan"
  head -c 31 seed > short
  ./signer --key-fd=3 3<short >/dev/null 2>&1; check "$?" "2" "a short seed is refused"
  # Exec stays refused beside a secret.
  printf '[launch]\nversion = 1\n[flags]\nkey-fd = { type = "fd", secret = true }\n[secrets]\ntargets = { use = "sign/witchy-2/targets", from = { from = "arg", name = "key-fd" } }\n[exec]\nexec = { dir = "/w/bin", programs = ["greet"] }\n' > sx.grants.toml
  printf 'fn main(out: Console<Write>, targets: Signer, exec: Exec):\n    out.print_int(7)\n' > sx.wy
  if $W build sx.wy -o sx --target=linux-arm64 --manifest=sx.grants.toml >/dev/null 2>&1; then echo "FAIL Exec beside a secret was built"; fail=1; else echo "ok   Exec beside a secret is refused when built"; fi
fi

# The broker's confinement itself (Landlock per root and port, seccomp), when the sources are
# mounted at /src.
if [ -n "${WITCHY_RUNTIME_RUST_ARCHIVE:-}" ]; then
  cc -Wl,--gc-sections -O2 -o confine-probe /w/confine-probe.c "$WITCHY_RUNTIME_RUST_ARCHIVE" && cp -R notes /w/notes 2>/dev/null
  check "$(./confine-probe)" "confined" "the broker's Landlock and seccomp refuse what is not granted"
fi

# A Listener: the planned address only, served by the confined broker.
build listener
./listener > listened &
lpid=$!
sleep 1
r=$(perl -MIO::Socket::INET -e '$s = IO::Socket::INET->new(PeerAddr => "127.0.0.1:47021") or die; print $s "a"; local $/; print scalar <$s>')
wait $lpid
check "$r $(cat listened)" "b 97" "a Listener accepts on its planned address"
exit $fail

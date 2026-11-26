# Native ARM64 builds

`witchyc build FILE -o EXECUTABLE --target=macos-arm64` uses stock Cranelift
0.136.3 to emit ARM64 Mach-O and the Apple toolchain to link it. Install the
macOS Command Line Tools or Xcode on the build machine. The linked executable
runs with macOS system libraries. On Linux ARM64 with glibc,
`--target=linux-arm64` emits ELF using the same Cranelift backend and links with
the system `cc`. The Rust runtime uses the Linux Landlock and seccomp ABIs;
linking an executable requires a system toolchain. Build on the host of the
selected target. The default build emits Wasm via `witchyc build FILE -o MODULE.wasm`.
Linux uses the same fresh Lean checks and policy pin format, with the RustCrypto
SHA-2 provider for compiler digests.

## Compile and run

From the repository root, with an owned target directory and a compatible
[Wasmtime host build](wasm-production-build.md):

```sh
mbx build --release --locked --target-dir "$COMPILER_TARGET"
witchyc="$COMPILER_TARGET/release/witchyc"
work=$(mktemp -d)
cp examples/native-scalar.wy "$work/scalar.wy"

"$witchyc" build "$work/scalar.wy" -o "$work/scalar" --target=macos-arm64 --measure
"$work/scalar" --output
"$witchyc" build "$work/scalar.wy" -o "$work/scalar.wasm"
"$WITCHY_WASMTIME_HOST" "$work/scalar.wasm" --output

sed -i '' 's/value + 1/value + 2/' "$work/scalar.wy"
"$witchyc" build "$work/scalar.wy" -o "$work/scalar" --target=macos-arm64
"$work/scalar" --output
"$witchyc" build "$work/scalar.wy" -o "$work/scalar.wasm"
"$WITCHY_WASMTIME_HOST" "$work/scalar.wasm" --output
```

Both targets print `42` before the edit and `43` afterward. With a trusted Lean
4.36.0 installation selected by `WITCHY_WASM_LEAN_ROOT`, or with a configured
ready binding, build the existing [Tape example](../tests/fixtures/slices/counted-projection.wy):

```sh
cp tests/fixtures/slices/counted-projection.wy "$work/tape.wy"
"$witchyc" build "$work/tape.wy" -o "$work/tape" --target=macos-arm64 --measure
"$work/tape" --output
"$witchyc" build "$work/tape.wy" -o "$work/tape.wasm"
"$WITCHY_WASMTIME_HOST" "$work/tape.wasm" --output

cp "$work/tape" "$work/tape.good"
sed -i '' 's/invariant used <= 4/invariant used <= 5/' "$work/tape.wy"
if "$witchyc" build "$work/tape.wy" -o "$work/tape" --target=macos-arm64; then
    exit 1
fi
cmp "$work/tape.good" "$work/tape"
"$work/tape" --output
```

The valid example prints `7`. The relaxed invariant allows a logical length of
five in a four-cell array, and the build refuses the `nopanic` claim. The
existing executable continues to print `7`. Replacing `ensures true` with
`ensures false` in the original example also produces `W0402` from proof
admission and preserves the old executable.

## Supported operations and trust boundary

Native supports unsigned and signed integer arithmetic, binary64 floating
point, Bool, control flow, ordinary and generic calls, fixed arrays, records,
and sums. It supports shared and unique borrows, slices and sub-slices, and
typed `Buffer<T>` storage. The internal calling convention carries scalar values in
64-bit registers. Addressable values have a separate memory layout. Unsigned integers
occupy their declared width, Bool occupies one byte, records align each field, and
arrays and buffers use the element's padded size as their stride. Slices carry an
address and a count and use this same stride for indexing and copying. Multi-lane
results use caller-owned storage. A sum reserves a word for its tag and whole-word
space for its largest flattened payload. Within that space the selected variant
has its ordinary memory layout, including when borrowed across a call. Native
uses the checker's bounds facts for counted walks and guarded slice reads. It
also uses admitted proofs for array indexes and slice reads when the proof
covers all accesses of that kind in the function. Other indexed accesses
keep their bounds checks.

Both targets use the same source graph and policy gate, checker for ownership
and borrowing, suspension checker, instance selection, and proof admission.
The native build reuses the existing Wasm-plan trap analysis to validate
`nopanic`, so a program making that promise must also fit the trap analyzer's
current Wasm planning limits. The compiler compares the source and policy again
before atomically publishing the executable. A refusal or linker failure
preserves the old output.

A build with `--manifest=GRANTS.toml` embeds a checked launch plan instead, and the
executable reads only the arguments that plan declares ([manifest-bound
executables](native-trusted-executables.md)): `./hello` prints with no flag. The flags below
are the development launch, for builds without a manifest.

The native runtime grants `Console<Write>` through `--output` and
`Allocator` through `--allocator`. A declared grant must be explicitly
enabled when launching the executable. The console supports decimal integer
lines, bytes, and low-byte-first words. On macOS, a declared `Console<Read>`
requires `--input-stdin` (also `--input`) or `--input=PATH`. A named path
must be a regular file reached without symlinks. The program reads through
a read-only stdin descriptor
(or a terminal reopened for reading) and supports both `next_byte` and
`next_run`. Other inherited input descriptors are closed before entry. A declared `Clock`
requires `--clock` and exposes wall-clock milliseconds since the Unix epoch
through `now_ms`. A declared `Rand` requires `--rand` and supplies
`next_u64` using macOS system randomness. Clock timers participate in native network polling.
A declared `Args` requires `--args=COUNT:BYTES`, with launch words supplied
as repeated `--arg=WORD` flags. The launcher checks UTF-8 and both ceilings
before entry. Other launch flags stay outside the capability.
A declared `Env` requires one or more `--env=NAME[=VALUE]` bindings.
Without an explicit value, startup reads the named variable from the parent
environment. The guest's `Env` can only look up the named bindings.
Grantable capability fields use `--grant=Type.field=VALUE`. The launcher
rejects malformed and duplicate bindings. Constructing the entry capability
requires a binding for each field.

On macOS, a positive `--deadline-ms=N` arms a wall-clock timer before
sandbox installation and Witchy entry. Expiry terminates the process with
`SIGALRM`, including when it is waiting for input.

### Endpoint-scoped networking

The [Redis example](../examples/redis/README.md) uses shared `Net` and owned
`Socket` on Wasmtime and macOS ARM64. Native launch accepts up to 16 comma-separated
endpoints with `--net=HOST:PORT,...` when `main` declares `Net`. The guest
retains its deny-default Seatbelt profile and sends bounded requests over a
private channel. The broker owns the TCP stream. It dials lazily using the endpoint index selected from the launch grant.
Guest requests carry no address. The launcher resolves names once before
sandbox entry and retains up to eight numeric answers per grant.

macOS also accepts `--listen=IP:PORT` for a declared `Listener`.
The broker binds on startup. Port zero selects an available port, and
accepted sockets report the bound port. Blocking accept waits up to five seconds.
Nonblocking try_accept returns WouldBlock if no connection is ready.
Native networking supports blocking and nonblocking TCP with up to 16 live streams per process
across both grants. Listener endpoints are explicit numeric IPv4 or IPv6 addresses. `Net.only` succeeds only for the exact granted
endpoint. Closing an owned socket invalidates its handle. A later connection
gets a fresh handle. Linux ARM64 still refuses native `Net`. Landlock's TCP
rules match ports and the current seccomp profile denies networking.

Native `File<Read>` uses an explicit `--file=PATH` startup grant on macOS.
Startup snapshots at most 1 MiB from a regular file, rejecting symlinks in
any path component, and closes the descriptor before entering Witchy.
The guest can read only the snapshot. Closing the capability releases it.

Native `File<Write>` uses `--write-file=PATH` on macOS. Write operations
buffer up to 1 MiB per launch. Closing commits via a broker-owned
descriptor-relative rename within the destination directory. Abandoning
the handle discards its contents. Existing nonregular targets are refused.

macOS `Dir<Read>` accepts a single `--dir=PATH` grant. The broker
retains the root descriptor and opens regular files relative to it.
It rejects absolute paths, dot components, traversal and symlinks.
It refuses a regular file with more than one link (status `InvalidPath`), for reads,
replacement, append and either side of a rename, as `--file` and `--input` do: a hard
link planted in a granted directory may name a file outside it.
Opened files support byte and bulk reads and can be closed independently.
Child directories can be opened and closed independently. Listings are sorted and capped at 1 MiB and 65,536 entries. A separate `--write-dir=PATH` grant supports rooted directory creation, writable child directories, buffered file replacement, exclusive creation, append, removal and rename.

A combined `Dir` uses `--dir=PATH --dir-rights=read,write`. Both rights
are backed by the same opened directory, and a combined child keeps
both rights when opened through the full capability.

Keep monotonic time distinct from the wall-clock observation exposed by Clock.now_ms. Capability reports
should identify requirements, launcher-supplied grants, effective authority,
and startup refusals.

The allocator owns a typed handle table. A handle identifies a slot and its
incarnation. A checked operation validates the handle and element type
before using its bounds or state. Guarded operations return the specified
error or the original owner on failure.

A standard `Vec<T>` holds a storage handle, length, and capacity. Its
allocation holds contiguous elements with an initialized prefix and tracks
that prefix with a length in runtime metadata. Spare capacity has no
per-cell metadata. `String` uses the same layout through `Vec<U8>`.
Replacement exchanges an initialized element in place, including when the
element owns storage. General `Buffer<T>` allocations support independent
cell initialization and use a bitmap.

The runtime checks state and extents for proof-admitted storage operations.
It bounds simultaneous allocations at 262,144 slots and live buffer storage
at 512 MiB, including payload, headers, and sparse occupancy bits.
Allocation may also fail when the system allocator refuses a request.

The native emitter, linker, and runtime are trusted for code and layout
correctness. Native artifact integrity depends on the compiler, linker, OS
and distribution path. Wasmtime independently validates Wasm and enforces
linear-memory confinement, fuel, deadlines, and output limits.

On both native targets, tasks keep their frames on a separate guarded stack.
Spawn copies the flattened task input, and a compiler-generated entry
adapter passes it to the selected Witchy function. Step and pause exchange
words on the launching thread. Close releases a finished task. Native
stacks have a 64 KiB minimum and a 1 MiB maximum, and the runtime supports
up to 256 live tasks. On Linux, the guest seccomp filter permits an
allocator-enabled program to remove access with `mprotect(PROT_NONE)` for these
guards. Adding access with `mprotect` is denied.

Native qualification compiled and ran all 25 successful fixtures under
`tests/fixtures/tasks` with expected output, including nested tasks,
cancellation, borrowed locals, floating-point values across a pause and
wide task frames. Both runtime refusal fixtures trapped. The network
timeout fixture also ran natively: the clock task won and the blocked
socket task handled cancellation.

Native `--data-only` applies the path and listing name rules to read-only,
write-only and combined `--dir=PATH` grants. Writable grants exclude
protected names and stage file writes in a private `.witchy-staging`
directory at the grant root. Native and Wasmtime hosts share the staging
lock and clean up abandoned temporary files before use. Native data-only
rename validates both paths and refuses directory trees containing protected
names, links, or entries on another device. The tree scan is bounded and
assumes the granted data root is not concurrently modified by an unrestricted writer.

Native `main(socket: Socket)` requires both `--listen=IP:PORT` and
`--instance-per-connection`. A supervisor runs one sandboxed child per
connection, serially. Each worker has a private broker channel, so an
interrupted request can be discarded and the listener can serve another
connection. No Witchy code runs in the supervisor. Use
`--connections=N` for a finite number of attempts and
`--connection-deadline-ms=N` for the worker timeout (default 30 seconds).
`--deadline-ms` bounds the whole serve loop, including idle accepts, and
can shorten an individual worker’s remaining time.
On macOS a worker's connection deadline starts once it has installed its
sandbox, just before its guest code runs: Seatbelt's install takes a few
milliseconds idle but most of a second on a loaded host. The install has its
own bound (10 seconds, never past `--deadline-ms`). A worker that cannot
install its sandbox, or does not in time, fails that connection alone: the
connection is closed, one line on standard error says so, and the next
connection is served. An install that fails (rather than runs slow) three
times in a row stops the service. On Linux the connection deadline is armed just before
Landlock and seccomp, which install in microseconds; the worker's filter
allows no timer call afterwards.
The `--net-bytes` and `--output-bytes` budgets apply across all workers.
Use `--net-bytes-per-connection` and `--output-bytes-per-connection` to cap
each worker independently within those run-wide budgets.
Directory grants (`--dir`, with `--dir-rights` or `--write-dir`, and `--data-only`) are
opened once by the supervisor and handed to each connection's broker. The file byte budget
(`--fs-bytes`) and entry budget (`--fs-entries`) are the run's, in memory every worker and
broker shares, as the output and network budgets are: serving another connection never
spends a fresh budget. File, input, outbound network and `Exec` grants are refused in this
mode.
`--waiting-connections=N` configures the accepted-socket queue (default 16,
maximum 4096, subject to process descriptor limits). The Wasmtime server's
full budget profile remains open.

With `--output --stream-output --plain-output`, native output prefixes every
line with `| ` and escapes bytes outside printable ASCII other than tabs and
newlines. Wasmtime preserves safe Unicode and marks only host-like lines.
Output budgets count guest bytes before escaping.

Native launches accept explicit `--unmetered` for manifests that select
unmetered execution. Native execution uses wall-clock deadlines and
capability-specific I/O limits. Numeric instruction fuel remains a Wasmtime
feature and is outside native parity scope.

Public SHA-256, SHA-384 and SHA-512 use macOS CommonCrypto by default. A compiler
built with `WITCHY_NATIVE_AWS_LC_ROOT=/absolute/install` uses AWS-LC for
these digests and the `Rand` capability, and supports public ML-DSA-65 and ML-DSA-87 verification. The
`witchy-2` profile uses ML-DSA-65. It requires the matching prefixed AWS-LC
headers under `include` and `libaws_lc_fips_0_14_4_crypto.dylib` under
`lib`. AWS-LC 4.2.0 rejects a static FIPS build on macOS.

On macOS ARM64, the pinned AWS-LC provider also supplies the AES-GCM
record operations and the 48-byte constant-time tag check. The Witchy
`tls_record` module owns per-direction sequence numbers and performs
TLS record framing on both macOS ARM64 and Wasmtime.

`import key_agreement` uses the same provider for ephemeral P-384
agreement. Generating a key requires `Rand`. The runtime holds up to
16 unfinished keys per guest process. An application can publish a
key share, then consume the opaque key by finishing agreement or
closing it. A rejected peer point leaves the shared-secret output
intact.

The [HTTPS client example](wasm-tls.md#run-the-client-example) verifies
the certificate path with explicit roots, DNS identity and clock time.
It shares the bounded certificate verification policy with Wasmtime
and uses the same TLS 1.3 and HTTP/1.1 Witchy source.

Use an operator-controlled installation directory that is also present on
the deployment machine. Native binaries that use the provider record that
absolute library directory as their runtime search path. The build does
not provision or validate the installation's provenance. The provider
must report FIPS mode before a crypto operation proceeds. This carries no
FIPS module validation claim for the deployed executable.

The native compiler and sandbox ran the shared ML-DSA-65 fixture with
this configuration. The expected vector and role verified, and the
altered and truncated cases matched the shared refusal output.

With AWS-LC enabled, native `Signer` binds a 32-byte seed from
`--secret-slot=PARAM:sign/witchy-2/ROLE:fd:N` before entering the sandbox.
The compiler emits the checked parameter names, and the runtime refuses
missing, duplicate or unknown bindings. The seed is cleansed after key
construction. The handle can export the public key and sign messages
beginning with the granted role's NUL-terminated domain. Native signing
admits descriptor sources only. Beside any `--dir` or `--write-file`, the broker
compares every file it opens to read or to append with the secrets' objects on the opened
descriptor, before a byte moves, and stops the run on a match; replaces and creates write
new files, and a replace, rename or remove of a name that is a secret's object is refused
(docs/native-trusted-executables.md, "Secrets beside file grants"). A secret refuses `Exec`.
`--secret-object=DEVICE:INODE` adds the identity of a file a
launcher read a secret from, which no grant may then reach. A `File<Read>` or `Console<Read>` beside a
secret is allowed when its opened object (device and inode, so names and hard
links do not matter) is no descriptor source of a bound secret. The runtime
checks this before entry. A launcher that reads a key file and passes it on a
pipe must not also grant that file, since the runtime sees only the pipe.

Fixed `Secret<Reveal>` parameters can bind descriptor sources with
`--secret-slot=PARAM:reveal-bytes:fd:N`. Reveal slots and signers share the
source identity checks and startup read deadline. The same directory, file
and console input rules apply.

Fixed `Secret<Seal>` parameters can bind a descriptor with
`--secret-slot=PARAM:seal:fd:N`. The native runtime keeps an AWS-LC
HMAC-SHA-256 context per slot and resets its message state after each tag.

Reveal and seal slots also accept `env:VARIABLE`. The launcher consumes
the variable, and refuses a matching `Env` grant regardless of flag order.
Signer seeds remain descriptor-only.

Native `SecretStore` supports named reveal-and-seal and seal-only grants,
using environment, explicit descriptor, or inline sources. Each successful
lookup has independent HMAC state. The store admits up to 64 bound names and
128 successful lookups per run.

Native `Exec` uses `--exec=DIR:name,name` (and `--exec-bytes=N`, default 1 MiB of
program output per run) on macOS, with the Wasmtime host as the reference. The broker
opens the directory and each granted program in it when it starts, without following a
link; a program that is missing then, not a regular executable file, or over 256 MiB is
`NotFound`. A run executes a private copy of exactly that opened file from a fresh
mode-0700 directory under the user's temporary directory, with the arguments as given
(at most 64 of 4096 bytes), an empty environment, `/dev/null` as input and error output,
no other descriptor, and its own session and process group (`setsid`). It is stopped
after two seconds or at the run's deadline, whichever is sooner, and whenever it exits, so
nothing it left holds the output open. Stopping it stops, in this order: every process still
its descendant (walked by parent pid while the program is unreaped, each checked to still
have the parent it was listed under), every live process in its session (all pids listed,
`getsid` equal to the program's pid; each round stops every member before killing any, and
repeats until a round finds none), its group, then the program; only then is it reaped.
While the program is unreaped (alive, or a zombie the broker has not collected) its pid, so
its session and group number, cannot be reused: nothing is signalled by number after
reaping. This catches a descendant that leaves the group (`setpgid`) and is orphaned at once.
Each process is signalled only while it is
listed under its parent; one that exits and whose pid is reused between that check and SIGSTOP
receives SIGSTOP and then SIGCONT (an owner's own stopped process would be resumed): macOS gives
no pid handle that would close this, and it needs a pid reused within microseconds.
What is not caught, precisely: a descendant that starts a session of its own (`setsid`) and
whose parent exits before the program is stopped. macOS gives an unprivileged process no
handle that follows it: `EVFILT_PROC` `NOTE_TRACK`/`NOTE_CHILD` are unsupported since 10.5
(`sys/event.h`), `NOTE_FORK` reports a fork without the child's pid, there is no
subreaper, and `proc_listchildpids` of a dead parent is empty. Re-checked (round 4): spawning
with `POSIX_SPAWN_SETSID` only makes the program itself a session leader, and a watch of
`EVFILT_PROC` `NOTE_FORK` on each process, adding children as forks are reported, still has to
list a forking parent's children after the event arrives; a parent that forks and exits first
has already handed its child to launchd, where nothing identifies it once it has left the
session. The gap is narrowed by no unprivileged mechanism, so the limit stays documented. The same residue as the
Wasmtime host; a process leaving its session is also outside every check a later run makes.
A broker killed after its grace period cannot remove its private directory: its supervisor
removes it while the broker is unreaped, and every run with Exec removes, at start, those left by
brokers that no longer exist (only directories of this user, mode 0700, no link, named exactly as
a broker names them; a live pid's is left alone). If the broker itself is killed mid-run, the
program loses its supervisor; the supervisor of
a socket-entry server therefore closes a stopped worker's channel and gives its broker time
to stop the program before killing it. The private directory is removed with
whatever the program left in it. The broker refuses any write through a directory or file
grant that would touch a granted program's opened file, and a secret refuses `Exec` (a
program runs outside the sandbox with the user's authority). When the guest stops (its
deadline, a trap), its channel closes and the broker stops the running program as above. The exec directory may
not contain, or be inside, a writable directory grant. `only` narrows an `Exec` to one
granted name. A socket-entry server opens the programs once in its supervisor; each
connection's broker runs copies, against the run's shared output budget.

Native Linux currently implements a smaller subset.

`witchyc build-session FILE -o EXECUTABLE --target=macos-arm64` accepts the
same `build REQUEST_ID SOURCE_SHA256` requests as the Wasm build session.
It retains the configured proof checker across builds and responds with
`witchy.native-build.v1` measurement records. Each accepted request still
runs native code generation and linking.

A macOS session smoke check with the fresh Lean checker built two revisions
of a function with a verified return-value contract. Each build reported
one Lean invocation, and the executables printed `41` and `42`. A third
revision with a matching source digest and a false contract was refused
with W0402, leaving the second executable byte-identical. This checks
fresh proof admission in the resident compiler process.

The same three-request check with a configured `body-interface-v2` binding
also admitted the two valid edits and refused the false claim. The first
request reported two SDK installation snapshots (1.94 s combined); the
second reported zero. Each valid request checked one goal, so this
demonstrates reuse of the validated SDK generation while still checking the
changed proof. The refused request left the previous executable intact.

### Boxes and weak links

Native `Boxed<T>` owns one typed allocation. `Weak<T>` holds its slot number
and generation without owning anything. The runtime checks the current
generation, element type, and initialized cell before resolving a link
to a machine address. The compiler still requires the same ownership,
loan, and proof evidence as on Wasm.

`memory.box_address` and `memory.weak_bits` expose the native slot number
as `U32`, with zero for null. These diagnostics are target-relative.
Wasm exposes a linear-memory offset instead. The native debug formatter
prints the slot number. Neither formatter promises a stable identifier
across reuse.

## Native startup sandbox

Both native targets install their sandbox before entering Witchy code. They
use the capability set from the checked `main` signature and the caller's
explicit launch flags. Missing required flags or failed sandbox setup prevent
entry. The OS loader and native runtime startup are trusted.

### Linux ARM64

```sh
mbx build --release --locked --target-dir "$COMPILER_TARGET"
witchyc="$COMPILER_TARGET/release/witchyc"
"$witchyc" build examples/native-scalar.wy -o ./scalar --target=linux-arm64
./scalar --output
```

- Landlock ABI 3 or newer denies new filesystem access covered by its rights.
  The ruleset grants no paths.
- Seccomp permits process exit. `Console<Write>` adds writes to inherited stdout.
  `Allocator` adds heap growth, unmapping, and private anonymous read-write
  mappings, and removing memory access for task stack guards. Filesystem
  syscalls, executable mappings, new sockets, and process or thread creation
  are denied in the guest.
  Inherited descriptors above stderr and stdin are closed before Witchy starts.
  Stderr is used for startup diagnostics. The caller chooses the stdout sink.

The Linux kernel must enable Landlock and seccomp and any outer sandbox must
allow their installation. The dynamic loader and runtime initialization are
trusted startup code. Launch with trusted libraries and loader settings. This
policy limits OS operations. CPU time and output size need separate resource
limits. See the kernel documentation for [Landlock](https://docs.kernel.org/userspace-api/landlock.html)
and [seccomp](https://docs.kernel.org/userspace-api/seccomp_filter.html).

### macOS ARM64

The linked executable installs a deny-by-default Seatbelt profile in process.
It blocks opening files, using network endpoints, and creating child processes.
The runtime closes inherited descriptors, retaining only stdout for a granted
`Console<Write>`. It accepts a write-only sink, or reopens a read-write
terminal for writing and verifies that it is the same terminal. For a duplex
redirection, use a write-only pipe such as `./scalar --output | cat`.

This uses the deprecated standalone custom-profile interface, tested on macOS
27.2.1 with SDK 27.0. If the interface or required descriptor cleanup fails,
startup refuses execution. Apple's entitlement-based App Sandbox has different
defaults. Seatbelt controls resources through its own policy hooks. Darwin
memory allocation is available to the trusted runtime, with Witchy's checked
grant controlling the language's allocation operations. [Apple App Sandbox](https://developer.apple.com/documentation/security/app-sandbox).

### Tightening after setup

Linux can add restrictive Landlock layers and seccomp filters to an existing
policy. A future source-level `lockdown` can use an explicit handoff to a
continuation whose checked capabilities describe its upper bound, with actual
retained handles supplied by the trusted runtime. Deferring the final profile
would still install a bounded setup profile at startup. Setup could read an
explicitly granted config file, close its handle, and enter the continuation
with the parsed configuration and console output.

A type describes operations, while a launch grant identifies resources such as
paths and descriptors. Reapplying Landlock leaves open file descriptors usable
for existing access, so the transition must close relinquished handles and
exclude live borrows or copies. The policy also has to allow cleanup of retained
values. For example, a buffer may need to free its storage after setup completes.
The setup profile would permit installing the final layer, which can revoke
that permission. A threaded implementation has to enforce the transition across
the participating threads. The current runtime installs the startup policy on
its only thread and denies thread creation. The typed handoff and native file
grants are future work.
[Landlock kernel API](https://docs.kernel.org/userspace-api/landlock.html),
[seccomp kernel API](https://docs.kernel.org/userspace-api/seccomp_filter.html).

The macOS probe could install a setup profile but a later call with a different
profile returned `EPERM`. A trusted launcher could start a separate worker
under the final profile and let it wait for the setup result and retained
handles. The transition would close setup-only handles and end the setup
process. Passing state across that boundary and exposing the source-level
transition remain open.

## Timing and test commands

```sh
python3 benchmarks/run.py --compiler "$witchyc" --target macos-arm64 --runs 5
python3 scripts/bench-native-go.py "$witchyc" --samples 9 > "$work/native-go.json"
mbx test --profile suite --locked --offline --target-dir "$COMPILER_TARGET"
```

The application runner builds the nine existing Witchy and Go benchmark
sources, checks their stdout byte for byte, and reports launch-through-exit
medians. The scalar build benchmark measures from process invocation to executable publication and
times launch through exit separately. Clean samples remove both output files,
use a fresh Go build cache per sample, and retain warmed OS caches. Warm edits
start with one primed Go build cache and use a distinct edit to the called leaf
per sample. The script checks exact stdout for every executable. Both builds
use ordinary one-shot commands. Witchy runs source checking and any required
proof work per invocation. The scalar sample has zero Lean calls. Native
currently recompiles every selected instance on each build. `--measure`
reports nanoseconds for each phase and the actual instance and Lean counts.

For byte-processing loops, record retired instructions per input byte alongside
elapsed time for the same phase. The [string benchmark](../benchmarks/witchy/str.wy)
prints its byte length after construction and its checksum after scanning. Sample
the process instruction counter and clock at those two print calls, then divide
the instruction delta by the printed length. At `3b0b5421` on Apple M4, the checksum
retired 115.4 instructions per byte and took about 47 ms for 10 million bytes.
Counting the successful path in disassembly, including library and runtime calls,
gives 115 instructions per byte, down from 142.

With packed native storage, building the same 10-million-byte string on M4 took
80 ms from an empty allocation and 31 ms with
`String.with_capacity(&allocator, 10_000_000)`. The earlier layout took 88 ms
and 33 ms respectively. Those are construction-phase medians from eight runs
per configuration. At that exact capacity, the payload, occupancy bitmap and
header together fell from 81,250,040 to 11,250,040 bytes. The checksum after
construction retired about 116 instructions per byte on the packed layout.

### First measurement

Apple M4, macOS 27.2.1, release Witchy built with Rust 1.100.0 and MBX 1.29.1,
Cranelift 0.136.3, Apple clang 21.0.0 and ld 27037.1, and Go 1.27.3. Nine
paired samples in each mode, alternate order. Times are milliseconds, median
with min–max in parentheses.

| Build mode | Witchy build | Go build | Witchy launch through exit | Go launch through exit |
| --- | ---: | ---: | ---: | ---: |
| Clean | 56.5 (54.3–59.6) | 2035.3 (1990.7–2198.3) | 5.6 (5.4–6.7) | 11.8 (11.1–12.7) |
| Warm leaf edit | 70.8 (55.1–77.8) | 161.1 (143.5–203.8) | 127.2 (5.9–181.9) | 259.8 (247.5–301.2) |

The clean Go build includes rebuilding its standard-library dependencies. Both
outputs matched for all 18 pairs. The warm build compiled two Witchy functions
per edit. Its median phases were 0.83 ms across source read, parse, checks, VC
generation and instance planning, 0.003 ms in proof admission, 0.26 ms across
CLIF lowering and Cranelift codegen, 0.36 ms writing the objects, 55.56 ms from
linker invocation through exit, and 0.37 ms for final recheck and publication.
Wall time also includes compiler process startup and time outside the measured
phases. Repeated launches use the clean sample's identical source, while warm
edits launch different executables. The launch column includes macOS process
startup and the single printed result.

The successful Tape build compiled five functions and used one fresh Lean
invocation. Its proof phase took 5.32 s, including 4.42 s for the two trusted
Lean installation hashes and 0.89 s from Lean spawn through exit. Its link
took 47.7 ms. The nearest opportunities are reducing Apple linker driver
overhead for small scalar builds and amortizing SDK validation through the
existing trusted proof session for proof-bearing edits. Caching unchanged
functions would save only a fraction of a millisecond on the measured scalar
program.

Embedding the fixed runtime object in the compiler reduced median link-stage
time from 80.7 to 50.3 ms in a separate nine-pair scalar run. The compiler
test suite, with its Rust test binaries already built, took 2.78–2.95 s over
three complete runs through MBX's `suite` profile. Each run passed 447 tests
with one existing ignored test. Fresh Lean and Wasmtime demonstrations were
measured separately.

Two environment variables help when measuring native code. `WITCHY_NATIVE_NAMES=1` at build
time names each function's symbol `witchy_function_N_Name` (for `sample`, `otool -tv -p`);
without it symbols are numbered only. `WITCHY_COUNT_ALLOCATIONS=1` at run time makes the
runtime print `allocations: N` on standard output when the program returns: how many
allocations it asked for.

## Buffer values and the three builds

Every buffer value (a sparse `Buffer`, a `PrefixBuffer`, a `Boxed`, a `Weak`) is a slot
number with a generation in the high word, and every access looks the slot up and compares
the generation, in compiled code or in the runtime. Whether a buffer is dense is its static
type. A default build writes proved accesses in line and keeps, for a `PrefixBuffer`, the
slot and generation test, the prefix rule and its bookkeeping. `WITCHY_NATIVE_CHECKED=1`
sends every access a proof admitted through the runtime's tests. `WITCHY_NATIVE_OBSERVED=1`
emits the default build's inline code and before each dense access calls `witchy_observe`,
which judges the access (type, capacity, prefix rule) and returns the address the inline
code must arrive at. The table of what each build tests is in `docs/decisions.md` (the
entry that restores slot and generation for prefix buffers in every build).
`scripts/native-checked.py` builds every fixture all three ways;
`scripts/native-vec-fuzz.py` runs random `Vec` operations on all three (and, with `--guard`,
the default build under libgmalloc) against a model. `WITCHY_REPORT_LIVE=1` with
`WITCHY_HEAP_AUDIT=1` prints the bytes still allocated when a run ends.

## Full native qualification on macOS ARM64

From `witchyc/`, build with `mbx build --release --offline --target-dir
/absolute/owned-target` and set `WITCHYC` to that target’s `release/witchyc`.
Set `WITCHY_WASM_READY_BINDING` to the [selected proof checker binding](wasm-production-build.md). For a
cold and warm comparison, start with an empty directory and keep it
for the second invocation. Use a directory created for this purpose,
owned by the account running the compiler.

```zsh
qualification_cache=$(mktemp -d)
fixtures=("${(@f)$(find tests/fixtures -name '*.wy' | LC_ALL=C sort)}")
git rev-parse HEAD
sysctl -n hw.logicalcpu
for cache_state in cold warm; do
  uptime
  result=0
  /usr/bin/time -p python3 scripts/native-checked.py \
    --compiler "$WITCHYC" --binding "$WITCHY_WASM_READY_BINDING" \
    --jobs 4 --proof-cache "$qualification_cache" --measure "${fixtures[@]}" \
    >"qualification-$cache_state.out" 2>"qualification-$cache_state.timing" || result=$?
  printf '%s exit=%s\n' "$cache_state" "$result"
done
```

Both invocations run the four false-proof controls with separate caches.
Check both exit statuses and keep the output files with the timing logs.
`--private-proof-cache` instead starts with an empty directory that the runner
removes at exit. A small cold admission check uses that option with
`--jobs 1 --measure --no-control tests/fixtures/native-checked/stale-link.wy`.
Check the three `lean_invocations` values in mode order. The recorded run had
one invocation in default and zero in checked and observed. The check took
0.89 s.

At `465e9b0c34a0a891e498c9c02f07e22fb45c5e7e`, four workers on a
10-logical-CPU macOS host evaluated 2,232 fixture paths. The cold run took
551.27 s at a starting load of 20.14 / 31.37 / 23.94. The warm run took
194.78 s at a starting load of 14.40 / 19.79 / 20.92. Observed
`programs/heap-full.wy` reached the 60 s run limit cold and completed in
36.12 s warm. It was the only per-mode status difference. A second warm run
took 201.25 s at a starting load of 14.54 / 16.89 / 18.33 and matched
every fixture result, per-mode status, and control outcome.

The warm run passed 1,068 source refusals, 877 output fixtures, and 57
trap fixtures. For each mode, 964 fixtures built, 60 listed builds were refused,
886 executions returned zero, 57 trapped, 18 had the expected launcher
refusal, and three ended with the expected signal. The 1,068 ordinary source
refusals were checked at the default build as usual. The run preserved the
six listed differences, three listed acceptances, one skip, and 139 imported
files without expectations. All four false-proof controls passed.

The original runner from `a22de9523` took 250.85 s with the same patched
compiler, fixture list, cache, and four workers at a starting load of
7.44 / 13.53 / 17.83. Its fixture results and false-proof control output
matched the instrumented warm run after stripping retry notices. Both
runners also returned the same failure for injected build and run timeouts.

Cumulative worker seconds from the compiler and runner logs are shown below.
Build phase measurements cover successful builds. Elapsed wall time also
includes failed builds and the controls.

| Run | Proof | Parse, type check, and VC | Code generation | Link | Fixture execution | Retry waits |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Original runtime, fresh cache, 8 workers | 937.09 | 69.69 | 37.05 | 607.84 | 484.69 | 1,018.38 |
| Patched runtime, cold cache, 4 workers | 300.34 | 50.07 | 11.84 | 325.04 | 679.31 | 28.05 |
| Patched runtime, warm cache, 4 workers | 10.56 | 43.35 | 10.58 | 266.74 | 209.75 | 28.05 |

The original runtime accounted for 900.30 worker seconds of retry waits in
`net/` fixtures. With immediate loopback listener rebinding, the cold and warm
runs each waited about 10 s for two retries of `net/write-stalled.wy`. It
retained its expected deadline signal in every mode.

Linking was the largest compiler phase in the warm run and accounted for
37% of total measured worker time. A 12-pair startup probe using a minimal
object and the runtime archive took a median 95.7 ms through Clang and 54.8 ms
when replaying the exact linker command. Calling the resolved Clang binary
directly selected a different deployment target and omitted the SDK root.
The qualification result uses the system driver and its existing toolchain
selection. The compiler interface is unchanged.

### Faster repeat qualification

On macOS ARM64, use the same warmed proof cache and a previous
`--measure` log to put long fixtures first:

```zsh
/usr/bin/time -p python3 scripts/native-checked.py \
  --compiler "$WITCHYC" --binding "$WITCHY_WASM_READY_BINDING" \
  --fast --proof-cache "$qualification_cache" --measure \
  --timings-from qualification-warm.timing \
  --parallel-fixture programs/heap-full.wy "${fixtures[@]}" \
  >qualification-fast.out 2>qualification-fast.timing
```

The fast runner defaults to seven fixture workers and retains all four
false-proof controls. Each resident compiler process has one fixed mode
and runs the ordinary build pipeline with a fresh proof session per request.
Clang supplies the native linker command for the run. For plain native
objects, the runner links an object once per sweep and reuses the result
when another build emits the same bytes. The link cache is private and
empty at the start of every sweep. Provider and sanitizer links keep their
existing driver path. The selected fixture may overlap modes only when
it has no launcher flags after Wasm metering flags are removed.

A seven-worker run on a Mac with ten logical CPUs took 91.33 s at a
starting load of 15.73 / 9.15 / 4.74. It reused 727 links and matched the
ordinary runner's fixture and per-mode outcomes, including all four
false-proof controls. In an earlier run, `net/cancel.wy` printed a shorter
received prefix than expected. The same mismatch reproduced with the
ordinary runner when the fixture was run on its own.

Pass `--linker /absolute/path/to/ld64.mold` with `--fast` to select a
Mach-O build of Mold. Keep the `ld64.mold` invocation name, including when
it is a symlink to `mold`. Clang still selects the SDK and derives the
link arguments. The alternate linker applies to plain native qualification
workers. False-proof controls, provider links, and sanitizer links retain
their existing driver paths. Each invocation starts with an empty link
cache.

## Native runtime sanitizer build

On macOS ARM64, use an isolated target directory and a compatible nightly Rust
compiler and upstream Clang. Both must agree on the ASan runtime ABI. Apple
Clang's ASan version marker may differ from upstream Rust's marker.

```sh
export WITCHY_NATIVE_SANITIZER_RUSTC="$(rustup which --toolchain nightly rustc)"
export WITCHY_NATIVE_SANITIZER_CLANG="$(brew --prefix llvm)/bin/clang"
mbx test --locked --offline --target-dir "$SANITIZER_TARGET" --test wasm_cli macos_native_
"$SANITIZER_TARGET/debug/witchyc" build examples/native-scalar.wy \
  -o "$work/scalar-asan" --target=macos-arm64
"$work/scalar-asan" --output --output-bytes=3
```

This instruments the embedded Rust runtime with ASan. The linker uses the
selected Clang's sanitizer runtimes. UBSan requires compiler instrumentation,
and the embedded runtime is now entirely Rust. Cranelift guest instructions
and handwritten stack-switch assembly are outside this instrumentation.

Nightly Rust can add its UB checks to both the compiler tests and the embedded
runtime. `WITCHY_NATIVE_UB_CHECKS` selects a diagnostic runtime archive that
includes Rust's core library for its check failure paths.

```sh
RUSTUP_TOOLCHAIN=nightly RUSTFLAGS=-Zub-checks=yes WITCHY_NATIVE_UB_CHECKS=1 \
  mbx test --locked --offline --target-dir "$UB_TARGET" --test wasm_cli macos_native_
```

Set `WITCHY_NATIVE_AWS_LC_ROOT` as well to include signer and secret-store
tests. These checks do not cover Cranelift guest instructions, system libraries,
or all possible undefined behavior.

The separate Miri harness uses the runtime's core sources directly, including an
allocation, transfer and release test. On macOS ARM64 with nightly Miri installed:

```sh
MIRIFLAGS='-Zmiri-strict-provenance -Zmiri-tree-borrows' \
  RUSTUP_TOOLCHAIN=nightly mbx miri test --offline \
  --manifest-path tools/native-core-miri/Cargo.toml --lib -- --test-threads=1
```

All 20 core tests passed with strict provenance and Tree Borrows on macOS ARM64. The allocator lifecycle and
overlap fixtures passed separately with strict provenance under both
Stacked Borrows and Tree Borrows. The allocator test under Miri compares the
ABI's integer address with the live allocation's address and reads via the
allocation pointer. The regular test still reconstructs a pointer from the
ABI address and reads through it. Native guest code, the OS adapters
and external cryptographic libraries still require separate testing.

## Native runtime core fuzzing

The standalone fuzz target drives dense and sparse occupancy transitions, slot
reuse, and budget accounting through the same Rust modules linked into native
programs. It also drives the real allocator ABI through valid sequences of
initializations, reads, removals, transfers and releases, comparing values and
occupancy against an independent model. It does not open files or sockets.
Build with coverage instrumentation on the harness crate, then run the driver.

```sh
mbx rustc --locked --offline --manifest-path tools/native-core-fuzz/Cargo.toml -- \
  -Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=3 \
  -Cllvm-args=-sanitizer-coverage-inline-8bit-counters \
  -Cllvm-args=-sanitizer-coverage-pc-table
tools/native-core-fuzz/target/debug/witchy-native-core-fuzz -runs=10000 -max_len=1024
```

The Kani harnesses check allocation size arithmetic, slot generation reuse,
budget debits, grant requirements, and dense and sparse occupancy transitions.
They also check buffer transfers, fills with aliased input, payload separation
from occupancy metadata, and the pointer returned by an admitted sparse-buffer
read.
With Kani 0.68.0 installed:

```sh
mbx exec cargo-kani kani --manifest-path tools/native-core-kani/Cargo.toml --default-unwind 16
```

The original 13 core harnesses passed on macOS ARM64. On the current Kani
rerun, 11 passed initially and both heap-fill harnesses passed after raising
their unwind bounds to cover the 16-byte scratch scan. The new slice-copy
harness passed separately. Occupancy checks cover up to three
operations on eight cells, and the multi-slot check uses two slots. These checks
cover the individual core functions and the states in each harness. The read
check uses an eight-byte buffer and replaces the hardware trap with a Kani
failure. The OS adapters and generated machine code are outside their scope.

The slice-copy harness checks the linked Rust copy routine on every valid source
and destination range in a modeled eight-byte allocation. It covers overlapping
and empty copies. A second harness verifies that a zero-length copy does
not access null pointers. For nonempty copies the C ABI caller supplies live
ranges and an initialized source. This harness provides no evidence about how callers derive those ranges.

Kani checks the stack layout routine used by task spawn for every admitted
size and 4, 16, or 64 KiB page sizes. It verifies the rounded stack size,
the two guard extents and page boundaries, and 16-byte initial stack
alignment assuming a page-aligned mapping base. Both the beginning and
end of the usable stack are strictly inside the mapping. A separate
harness checks rejection for a zero page size and representative
nondivisors of the guard size, including `usize::MAX`. Mapping behavior and
stack switching are separate from this arithmetic proof. With the
routine inlined, the optimized runtime instructions match the baseline
on macOS ARM64 and Linux ARM64 musl.

The task switch shares the initial x19, link register, and stack-pointer slots
between Rust and assembly. Compile-time Rust assertions check the context size
and alignment. A source check matched each save and restore slot for x19–x30,
d8–d15, and SP. Both native object disassemblies put the second register
of the x29/x30 pair at byte 88 in the source and destination contexts.
The emitted switch restores SP and then returns through the loaded x30. Stack allocation relies on libc returning suitably aligned
storage from `malloc`. Rust checks the task's 16-byte alignment and both
context offsets at compile time. The ARM64 glibc probe checks 16-byte
fundamental C alignment. Darwin ARM64's SDK reports 8-byte alignment for
`max_align_t`. Apple's allocator documentation independently specifies
16-byte minimum allocation alignment. [Apple's heap memory documentation](https://developer.apple.com/videos/play/wwdc2024/10173/).
The code also relies on a successful `mmap` supplying a page-aligned mapping
of the requested length. The OS must apply the requested protection to the
guard pages. These assumptions need platform evidence. The assertions do not
check actual guard-page faults, register preservation, or task reclamation.
The `wasm_cli::native_tasks` tests exercise a pause/resume cycle with live
locals, normal task completion, and a guard-page fault. These scenarios do
not independently inspect every saved register or cover nested task stacks.
Run the tests on each native host to exercise its libc and guard behavior.
A standalone fixture passed on macOS ARM64 and Linux ARM64 musl, including
a rerun after extracting the stack-layout routine. It resumed
a parent and child across nested stack switches, checked the values and
finished states, and reclaimed both task records. A separate completed task captured
its entry SP, which the fixture rounds up from the top writable page
to locate the mapping. On both targets, `mprotect` returned `ENOMEM`
for every page in the calculated mapping immediately after finish,
including the two guard regions. Separate processes verified that
attempting reclamation of a new, paused, or currently running task traps
on both targets. Another child process
reused a task slot and confirmed that its old handle traps while the new
handle remains valid. The stack-exhaustion child ended with the expected
protection signal on both platforms. Separate subprocesses each issued a
single byte load at the first address of the lower or upper guard and
observed the same protection signal. The fixture does not read the
address reported by the kernel for the recursive overflow. A fixture-only assembly wrapper also
observed that x19–x29, d8–d15, and SP retain their values across the switch
on both platforms with distinct seeded values on the two stacks. A macOS
scratch build with the d8/d9 save and restore removed trapped in this
check. A fixture-only round trip calls the real switch with two independent
contexts and an alternate stack. On both platforms it compares x30 on
entry and on resume with the expected instruction addresses, and checks
both contexts' saved x30 at byte 88. Its source is
`tests/fixtures/native-sandbox/nested-task-abi.rs`.
On both ARM64 targets, direct reads from the computed lower and upper
guard pages faulted. In separate subprocesses, making only the selected
page readable with `mprotect` let the same load return. This tests that
the direct reads fault because of the selected guard's protection.
The standard stack-overflow case checks the terminating signal. A
separate offline probe installs a C signal handler on an alternate
stack and compared `si_addr` with the lower guard range recorded
by the mapping wrapper. The recursive task overflow reached that
range on both ARM64 targets, with SIGBUS on macOS and SIGSEGV on Linux.
The fixture also fills all 256 task slots, checks that a further spawn
returns zero, finishes one task and replaces it with a distinct handle.
It checks that the table is full again and runs and finishes every
remaining task. This passed on both ARM64 targets. The test exercises
slot exhaustion. A separate scratch build redirected only the task
module's OS symbols and injected failures at `mmap`, the first
`mprotect`, and task-record `malloc`. On macOS and Linux ARM64,
each spawn returned zero, any mapping obtained before failure was
unmapped successfully, and a subsequent task ran and finished.
The extended probe also failed the second Linux guard setup after the
first succeeded and observed the same cleanup and recovery. On both
targets, injecting `munmap` failure when finishing a completed task
terminated the child with the runtime's trap signal. Injecting a failed
task-record allocation together with failed cleanup `munmap` during
spawn returned zero on both targets and left the mapping live until
process exit. The next spawn acquired a distinct mapping and finished
normally. The failed task publishes no handle, so this case leaks the
mapping without exposing it through a task slot.

To build the fault-injection fixture from `witchyc/`, make a temporary
copy of the task module and redirect its four foreign declarations.
The fixture's wrappers call the real libc functions except at the
selected failure point.

```sh
scratch=$(mktemp -d)
for file in tasks task_switch task_stack_layout global; do
    cp "runtime/native/$file.rs" "$scratch/"
done
cp tests/fixtures/native-sandbox/task-allocation-probe.rs "$scratch/probe.rs"
python3 - "$scratch/tasks.rs" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
source = path.read_text()
for symbol in ("mmap", "mprotect", "munmap", "malloc"):
    declaration = f"    fn {symbol}("
    assert source.count(declaration) == 1
    source = source.replace(declaration, f'    #[link_name = "probe_{symbol}"]\n{declaration}')
path.write_text(source)
PY
mbx exec rustc --edition=2024 -C opt-level=2 -C panic=abort "$scratch/probe.rs" -o "$scratch/probe"
"$scratch/probe"
```

To check the overflow address as well, compile
`tests/fixtures/native-sandbox/task-overflow-address.c` to an object with
the target C compiler. Add `--cfg overflow_address` and
`-C link-arg=/absolute/path/to/overflow.o` to the Rust command above.
The probe expects its overflow subprocess to report the fault address
inside the lower guard through a dedicated exit code.
Build it with `mbx exec rustc --edition=2024 -C opt-level=2 -C panic=abort`
and run the executable on the target. For a macOS-to-musl cross build,
add `--target=aarch64-unknown-linux-musl -C linker-flavor=ld -C linker=ld.lld`
with an equipped Rust toolchain and LLVM linker.
The Linux transport module's three tests also passed in the VM with musl.
They cover descriptor transfer with close-on-exec, malformed multi-descriptor
cleanup, and partial framing and EOF. The glibc header checks independently
cover the transport record layouts.

The macOS SDK declarations for `pollfd`, `timespec`, `rlimit`, `siginfo_t`,
and `nfds_t` can be checked without running a program:

```sh
xcrun clang -std=c11 -D_DARWIN_C_SOURCE -Wall -Werror -fsyntax-only \
  tests/fixtures/native-sandbox/darwin-abi-layout.c
```

The Darwin probe also checks the descriptor record and selector used for
`proc_pidinfo`. The runtime checks the returned byte count against the
allocation and requires whole records before forming a slice. This relies
on `proc_pidinfo` initializing the bytes it reports and on exclusive
ownership of the descriptor table during startup. The regular native
startup test exercises inherited-descriptor cleanup. It leaves malformed
or partial foreign replies outside its coverage.

Both header probes check the directory-stream signatures and the offsets of
the name and kind fields returned by `readdir`. The four broker callers close
their descriptor when `fdopendir` fails. On success the stream owns it and
`Entries::drop` calls `closedir`. The entry borrows the mutable stream until
the caller finishes with the name, so a second read cannot invalidate it
while borrowed. The temporary-directory cleanup and Linux child scan also
close their input descriptor on failure and close the stream on success.
These checks rely on libc preserving descriptor ownership on failure and
providing a live NUL-terminated name until the next read or close.

Both probes check the resolver record and function signatures. The Rust
resolver keeps the original list head, accepts only non-null address
pointers with lengths from 1 through 128, and copies each address into
its own 128-byte record before freeing the list. The source proof of that
destination bound still relies on libc returning a live, aligned list and
providing initialized bytes for each reported address length.

On Linux ARM64 with glibc, run:

```sh
cc -std=c11 -D_GNU_SOURCE -Wall -Werror -fsyntax-only \
  tests/fixtures/native-sandbox/linux-abi-layout.c
```

The probes check sizes, alignments, and selected offsets against the Rust
assertions in `runtime/native/sys.rs` and `runtime/native/linux_stat.rs`.
The Linux probe also checks the `openat2` argument layout, syscall number
and resolve flags against the Linux headers. The Rust layout assertions
compiled for ARM64 musl. The optimized `open_no_symlinks` wrapper emitted
identical instructions before and after the assertions.
The two Linux process-enumeration loops check the `readdir` result for
null, take a raw pointer to `d_name`, and copy the bytes they need into
a local path buffer before advancing or closing the directory stream.
Neither loop takes a reference to the full fixed-size Rust `Dirent`.
The offset checks establish where the name starts. The borrow relies
on libc supplying a readable NUL-terminated name for each non-null entry
and retaining that storage until the next read or close of the stream.
The clock and randomness adapter uses a second Rust timespec declaration
with compile-time size, alignment, and offset assertions. The C probes
check the realtime clock selector and the `arc4random_buf`, `getrandom`,
and pinned `RAND_bytes` signatures. The standalone
`tests/fixtures/native-sandbox/clock-random-abi.rs` fixture passed on macOS
ARM64 and Linux ARM64 musl. Build it with `mbx exec rustc --edition=2024
--test` and run on the target. This exercises the live clock and random
calls. An optional Linux fixture build with `--cfg witchy_test_wrapped_getrandom`
and GNU linker option `--wrap=getrandom` also passed. It forces an
`EINTR` followed by three- and five-byte reads, and checks the exact
returned bytes and requested lengths. Unscripted calls delegate to libc.

The deadline adapter checks its signal action, signal set and timer
record layouts against the Rust assertions and both platforms' C
headers. The macOS ARM64 setup-deadline fixture passed under Seatbelt,
including re-arming with the connection deadline. The standalone
Linux ARM64 musl deadline fixture also passed with live libc calls.

The broker resolver's `Address` and `AddrInfo` layouts are checked in
Rust and against both C headers. The existing endpoint fixture passed
numeric and localhost resolution on macOS ARM64 and Linux ARM64 musl.
The Rust loop skips null, oversized and unsupported address records,
copies each accepted address into its own buffer, and frees the
original list after traversal. It relies on libc keeping the list and
the advertised address bytes readable until `freeaddrinfo`.

The socket timeout fixture ran on macOS ARM64 and Linux ARM64 musl.
It reads back both five-second timeout values using the standard library
after the broker sets them through the runtime's C wrapper. Passing an
invalid descriptor also returned failure on both platforms.

The same fixture verified the byte I/O wrappers with a pipe, a file,
and a Unix socket pair. On both targets, a short read preserved the
unwritten tail of the destination buffer. The positional file read
also left the descriptor offset unchanged, and the plain reads and
socket receive returned zero at end of input.

The glibc 2.39 ARM64 headers require eight-byte alignment for `siginfo_t`.
The Rust mirror declares that alignment explicitly. A Linux ARM64 VM smoke
test (`tests/fixtures/native-sandbox/linux-waitid-abi.rs`) called the Rust
`waitid` wrapper against glibc and checked the PID and
exit status of a child that exited normally. A `WNOWAIT` call and the
subsequent reaping call both reported the same PID and status. A final
wait for the reaped child returned `-1` with `ECHILD`. With a child
held open by a pipe, `WNOHANG | WNOWAIT | WEXITED` returned success with
a zero PID, which callers treat as no event. A blocking wait interrupted
by a signal with restart disabled returned `-1` with `EINTR`. The probe
covers these wrapper calls. Caller retry behavior and other child state
changes need separate evidence.

To reproduce the smoke test, compile the fixture to a position-independent
ARM64 Linux object with `mbx exec rustc --crate-type lib --edition 2024 -C
opt-level=2 -C panic=abort -C relocation-model=pic --emit=obj`. On a cross
host, add the installed ARM64 Linux Rust target. Link the object with
`ld.lld -shared -o probe.so probe.o`. On the Linux ARM64 test host, run
`timeout 10 python3 tests/fixtures/native-sandbox/linux-waitid-abi.py ./probe.so`.
The shared object resolves the C calls through the test host's libc. The
driver compares device, inode, mode, link count, UID and size from both
Rust metadata wrappers against Python's `os.fstat` and checks the child
PID and exit status reported by `waitid`. It also checks that both metadata
wrappers return `None` for an invalid descriptor and that the directory-relative
wrapper returns `None` for a missing name.

On macOS ARM64, the separate `tests/fixtures/native-sandbox/darwin-waitid-abi.rs`
fixture passed against libSystem with its C driver. From `witchyc/`, run

```sh
scratch=$(mktemp -d)
mbx exec rustc --crate-type lib --edition 2024 -C opt-level=2 -C panic=abort \
  -C relocation-model=pic --emit=obj tests/fixtures/native-sandbox/darwin-waitid-abi.rs \
  -o "$scratch/probe.o"
xcrun clang -arch arm64 -std=c11 -Wall -Werror \
  tests/fixtures/native-sandbox/darwin-waitid-abi.c "$scratch/probe.o" \
  -o "$scratch/probe"
"$scratch/probe"
```

The fixture checks that a child held open by a pipe produces a zero PID
with `WNOHANG`, that the peek and reap both report the child PID after it
exits, and that waiting again returns `-1` with libSystem's `ECHILD`.
The fixture reads the error through the runtime's thread-local errno wrapper.

With `opt-level=2` and `panic=abort`, the native runtime text disassembly
matched baseline `1a66ce305` on macOS ARM64 and when cross-compiling an ARM64
Linux object with the installed musl Rust target. The Linux glibc probe above
separately checks the `waitid` ABI on that host.

A repeat after the heap proof and Linux layout additions compared the
full optimized runtime objects. Darwin instruction and relocation
disassembly matched. Linux instruction disassembly matched, with
relocation differences confined to offsets within the string data
section. Resolving those relocations to their NUL-terminated strings
produced identical disassembly after normalizing the absolute checkout
prefixes in source locations. Both objects used the default crypto
provider for their target.

With pinned AWS-LC enabled on macOS ARM64, both the instruction and
relocation disassembly match baseline `1a66ce305` after the task,
digest, P-384, and TLS proof changes. The P-384 and TLS record fixtures
passed against the latest compared object. The digest fixture passed
against the object compared after the digest proof changes.
The PKIX `X509_get0_pubkey` input was corrected to match the provider's
const pointer declaration. With AWS-LC enabled, optimized runtime
objects before and after that correction were byte-identical on both
ARM64 Darwin and the ARM64 Linux musl Rust target.

The default-provider comparison was repeated after the clock, deadline,
resolver and socket timeout layout checks. Both ARM64 runtime objects
still emit identical instruction disassembly to the baseline.

After compiling both variants, two alternating pairs of `mbx test --offline
--locked --release --no-fail-fast` with the same proof-checker binding ran on
macOS ARM64. Wall times were 13.05 s baseline and 10.45 s candidate, then
6.82 s candidate and 7.21 s baseline. Each run had the same 580 passing
tests and one ignored test. Neither pair showed a test-time regression.
The variation between pairs limits what can be inferred about speedup.
A repeat against the verification changes available at the time measured 7.06 s baseline
and 8.11 s candidate, then 7.20 s candidate and 7.56 s baseline. The
same test names and outcomes appeared in all four runs. The maximum
observed regression was 1.05 s.

After adding the word and slice proofs and the remaining Linux layout
assertions, two further alternating runs used the same baseline and
compiler source, with only the runtime changes applied to the candidate.
Baseline/candidate pairs were 7.07/10.23 s and 7.87/7.42 s, then
8.61/7.86 s and 8.33/7.32 s. All eight invocations ran the same
580 passing tests and one ignored test. The +3.16 s observation came
from the integration test binary on a busy host and exceeded the
budget. It did not recur in the other three pairs. The host was shared
and reported one-minute load above 30 on ten logical CPUs around these
runs, so the comparison does not isolate the cause of the outlier.

A follow-up with the task and digest changes used prebuilt release
artifacts and the same baseline and test identities. The first pair was
6.69 s baseline and 8.66 s candidate. The reversed pair was 6.68 s
candidate and 6.85 s baseline. The largest regression in this follow-up
was 1.97 s. One-minute host load was 24.89 at the start and 30.65 after
the run. The results did not identify a consistent overhead to offset.

A later refresh applied the current native runtime and PKIX declaration
to the same matched test checkout. With prebuilt release artifacts,
baseline/candidate ran in 8.07/7.33 s, then the reversed
candidate/baseline pair ran in 7.16/7.33 s. All four runs had identical
test names and outcomes, with 580 passing tests and one ignored test.
One-minute load was 18.29 before the four runs and 18.32 afterward.
Neither pair showed a regression to offset.

### Native runtime verification scope

Completed Kani runs verified 23 core harnesses, five global-state
harnesses, 26 runtime-logic harnesses and three Linux sandbox harnesses.
The combined logic run includes both timer ABI proofs. The argv bounds
proof was added afterward and passed separately.
The two initial heap-fill failures were unwind assertions. The
corrected harnesses passed with those assertions enabled.

| Boundary | Current evidence | Remaining obligation |
|---|---|---|
| Heap allocation, handles and raw copies | Kani core harnesses cover bounded layout, occupancy, aliasing and copy cases. | Establish raw-pointer preconditions at each ABI caller and cover cases outside the modeled bounds. |
| Global state | Kani exercises the table and in-place state operations sequentially. | Establish single-threaded use and exclude access from signal handlers or while an exclusive borrow spans a switch. |
| Task stacks | Kani verifies stack layout arithmetic. The macOS and Linux musl fixtures check register preservation, guard protection and overflow fault location, stack reclamation, slot exhaustion, and injected setup and cleanup failures. | The switching and overflow checks exercise selected call sequences. They do not prove all task interleavings or OS failure combinations. |
| OS and broker adapters | Selected Darwin and Linux layouts and signatures are compared against C headers. Runtime fixtures exercise child waits, Linux metadata and descriptor transfer. Kani logic and sandbox harnesses cover bounded parsing and state transitions. | Check remaining C signatures and layouts, Linux ABI behavior, initialized return lengths, and descriptor ownership through failure and cleanup paths. |
| Crypto | Kani proves digest length admission and early rejection. CommonCrypto and pinned AWS-LC fixtures exercise selected calls on macOS and Linux ARM64. | Check the remaining Rust pointer, buffer, ownership and callback contracts against the linked provider on each supported platform. |

The two direct task-switch call sites are `witchy_task_step` and
`yield_task`. Both pass raw pointers to the two embedded contexts and
retain a raw task pointer across the switch. The bootstrap passes a raw
pointer to the task input and creates no Rust borrow that spans the
entry call. The task status gates protect the mapping from reclamation
while a caller can still switch into it. This trace covers the references
created by the switch and bootstrap code itself.

The installed runtime signal handlers have separate state paths.
`request_stop` sets an atomic flag. `stop_serving` reads the atomic
child identifiers, performs child cleanup through libc, and exits.
`setup_expired` exits directly. These handler paths do not touch
`Global` or `State`. The runtime's other signal dispositions install
SIG_IGN or SIG_DFL, or restore a previous action in the broker. The
source inventory found no explicit thread creation in shipped Rust
under `runtime/native/`. Task switching occurs on the calling thread.
The process-wide single-thread assumption still depends on the linked
code and entrypoint. The shipped C-ABI `main` is defined in Rust and
enters the generated guest synchronously in the ordinary launch and
socket worker paths. An undefined-symbol scan of the tested ARM64
runtime objects and pinned AWS-LC artifacts found no direct imports
of pthread, C11, Mach, dispatch or clone thread creation entrypoints.
This checks those direct imports only. Indirect calls and the behavior
of system libraries remain assumptions. Tracing every `State::with`
closure through its callees also remains open.

The timer ABI confines its `TIMERS` borrows to table lookup, insertion
or retirement. It copies the deadline before writing to the ABI output.
The socket worker releases its `SOCKETS` borrow before calling
`witchy_entry`. The socket close and transfer paths retain a table
borrow during synchronous broker I/O and budget accounting. Those
callee paths use the broker transport and libc I/O, with no task switch
or guest callback in the worker. Group membership edits nest the
`SOCKETS` borrow inside `GROUPS`, and address assembly returns owned
copies of its temporary data. The Exec broker moves its state out of
`BROKER` before dispatch, so a fork during dispatch does not inherit
an active borrow of that cell.

The Linux child sweeps borrow their scratch arrays for one round.
The direct sweep borrows `PIDS` and takes `INPUT` during the proc-file
read, then takes `HANDLES` after the read ends. The descendant sweep
borrows `TREE`, `PIDS` and `BELOW` and uses `INPUT` during each
individual proc-file read. The nested cells are distinct. These
rounds use synchronous process queries, signals, waits and descriptor
cleanup, then release the borrows before sleeping. The macOS session
sweep holds `ALL_PIDS` during synchronous process queries and signals.
These paths issue no fork, task switch or guest callback with a
borrow live. The source trace relies on the foreign calls returning
through their documented interfaces and on the separate signal paths
described above.

Launch argument and policy grant helpers return copied bytes, scalar
results or failure. Manifest loading borrows `PLAN` while it reads
and binds launch data, including calls into separate argument and
grant cells, then returns the launch value before guest entry.
The directory dispatcher keeps its path borrow through synchronous
file and broker calls. The output opener uses the path as a readable
slice for that call and returns a handle. Exec program setup keeps its
borrow through descriptor operations and metadata queries. The guest
Exec dispatcher takes its owned mutable entry out of `HANDLES` before
using it, and the socket broker takes the shared mapping out of
`SERVER_ENTRIES` before using it. These observations cover source
paths and depend on the corresponding libc and provider calls
returning without re-entering the runtime.

The Exec environment cell returns only validation results. Its closures
call the name policy and inspect or copy bytes. The program cell returns
copies of its records or scalar metadata. Its descriptor export copies
descriptor numbers to caller-owned output. Budget reads and writes use
the stored raw pointer only while the shared word is mapped, which is
a separate caller obligation. No reference into either cell travels
with these results. The mutable Exec entry moved out by
`Option::take` points to a separate allocation, and the socket broker
moves ownership of the shared mapping rather than borrowing it from
the state cell.

For socket launches with Exec, the supervisor obtains the budget word
from `SharedWord::map` and transfers the mapping out of its owning
wrapper before storing the raw address. A worker unmaps its inherited
copy before guest entry and exits if that unmap fails. The supervisor's
mapping remains live. The worker therefore retains an unusable pointer
value in its inherited program table. The runtime's budget read and
spend calls occur in the Exec broker. Keeping those calls out of the
worker after unmap is a process-role invariant.

A Miri-only harness uses the real `State::with` under strict provenance.
It checks that same-state re-entry panics before running the inner
closure, that the outer reference remains usable when the harness
catches that panic, and that a later borrow observes the outer write.
The harness passed with Stacked Borrows and Tree Borrows. The shipped
runtime aborts on panic.

The timer ABI proof calls `witchy_timer_due` against arbitrary contents
of the eight-slot model. A matching nonzero handle initializes its
deadline output with the stored value. Every other handle returns
false even when the output pointer is null. A writable output pointer
remains a caller obligation on success.
A second timer harness exercises allocation, retirement, and slot
reuse through the real ABI. A retired handle returns false without
accessing a null output after its slot is reused. The replacement
handle writes the replacement deadline.

The Miri launch-argument fixture calls the actual bind, select and byte
operations. It covers an empty word, multibyte UTF-8, rejection of
invalid UTF-8, reads at word boundaries, overflow-sized indices and
clearing a selection after an invalid index. It starts with Rust-owned
C strings. The launcher separately relies on the C entrypoint's argv
pointer and string lifetime contract.

A separate Kani proof verifies `Argv`'s raw indexing for every index in
a two-entry caller-owned model, including null entries, negative counts,
and access beyond the declared list length. It also checks that the
text accessor returns early for those absent entries. Kani cannot model
the foreign `strlen` used by `CStr::from_ptr` for a present entry.
A separate Miri fixture checks that a present argv entry can be read,
then split through its raw pointer after the previous borrow ends.
Both substrings remain readable. This passed with strict provenance
and Tree Borrows.

Four launch-plan tests also passed in that mode, including reading
copied plan fields after dropping the input and taking a duplicated
descriptor only once. The Miri harness supplies a `strdup` shim backed
by its libc allocator because its Darwin emulation lacks that call.
The shim exercises the Rust ownership and pointer use with a distinct
allocation. The four ownership tests also passed against native libc
on macOS ARM64 and Linux ARM64 musl, using the real `strdup`. The
extended descriptor test also verifies that the copy is close-on-exec
and can read from the pipe after closing the original. That extension
passed on macOS ARM64 and Linux ARM64 musl against libc, and under Miri.

The file snapshot loader admits at most one MiB from the metadata and
allocates one extra byte to detect growth. Its cursor advances only by
positive libc read counts, which must be at most the remaining capacity.
It publishes only the bytes filled before EOF and frees the allocation
on failure. The snapshot ABI caps copies by the initialized bytes
remaining and the connection budget. A nonempty output must be writable
and disjoint from the runtime-owned allocation. This is a source trace
of the loader and snapshot copy. The live byte-I/O fixture checks a
short read and EOF on each target. The macOS native integration tests
also passed for an explicit read grant and a buffered snapshot copy.
The copy test checks binary payload preservation and refusal to commit
when the filesystem budget cannot cover the output. The standalone
Linux musl fixture exercises the Rust loader and snapshot ABI with a
real file. It checks successive partial and end-of-file reads, leaves
bytes beyond the returned count intact, accepts null output for empty
or EOF reads, and closes the snapshot. With a two-byte connection
budget and an eight-byte request, the first read touches only two
bytes. After replenishing the budget, the next read resumes at byte
two. All ten tests in the
`tests/fixtures/native-sandbox/linux-file.rs` harness passed together,
including the existing hard-link rejection and metadata tests.

The P-384 adapter gives AWS-LC a 97-byte output for the public point
and a 48-byte output for the shared secret. Both arrays are initialized
before the calls, and the adapter checks the returned lengths. Owned
key and point wrappers free the allocations on drop, including early
returns. Borrowed group and public-point pointers remain inside the
key operation. The slot table serializes its mutable access and moves
ownership out on take. These source checks rely on the pinned provider
honoring buffer capacities and its owned and borrowed pointer contracts.
The raw ABI output copies also rely on the emitter supplying writable,
non-overlapping output ranges. The C signatures were checked against the headers bundled with
`aws-lc-fips-sys` 0.14.4. Run the offline probe with
`clang -I "$AWS_LC_INCLUDE" -std=c11 -Wall -Werror -fsyntax-only
tests/fixtures/native-sandbox/aws-lc-abi.c`, with `AWS_LC_INCLUDE`
pointing to those headers. The probe also checks the HMAC context and
SHA-256 signatures used by the secret adapter, including the width
of the HMAC output length. It passed for Darwin and Linux ARM64.
The same probe checks all PKIX provider declarations on both targets.
The PKIX adapter passes each bounded DER slice to `d2i_X509` with a
null destination and a writable local cursor. It immediately wraps the
returned certificate in an owning guard, so trailing bytes and partial
chains release all certificates parsed so far. AWS-LC takes its own
reference when a root is added to the store. The intermediate stack
holds borrowed certificate pointers, and its destructor frees just
the stack. The verification context drops before the store, stack
and certificate owners on both success and early return. The adapter
uses the verified chain and its borrowed elements only while the
context is live, and uses the borrowed leaf public key only while
the leaf is live. The digest verification context drops before the
leaf. These lifetime checks use the pinned provider's ownership
contracts and Rust's local drop order. A scratch Linux ARM64 PIE
linked the pinned static provider and used linker wrappers around
`X509_STORE_new`, `OPENSSL_sk_new_null`, `X509_STORE_CTX_new` and
`EVP_MD_CTX_new`. With the PKIX known-answer inputs, it returned null
at each of the 36 observed wrapped calls in separate verification
attempts. Every attempt reached the injected failure and a subsequent
unmodified verification succeeded. A second run redirected only the
Rust runtime archive's references to those four constructors and their
matching destructors. Each constructor failure returned false, left no
tracked object outstanding, and was followed by a successful verification.
The tracking covers objects returned directly by those constructors.
Certificate reference counts and provider-internal allocations remain
outside that tracking.

To rerun the four-constructor sweep on Linux ARM64, run from
`witchyc/` with `WITCHY_RUNTIME_RUST_ARCHIVE` set to the AWS-LC-enabled
runtime archive and `AWS_LC_ROOT` set to the pinned provider install.
The symbol rewrites apply to a temporary copy of the Rust archive.

```sh
scratch=$(mktemp -d)
for symbol in X509_STORE_new X509_STORE_free OPENSSL_sk_new_null OPENSSL_sk_free \
    X509_STORE_CTX_new X509_STORE_CTX_free EVP_MD_CTX_new EVP_MD_CTX_free; do
    printf 'aws_lc_fips_0_14_4_%s __wrap_aws_lc_fips_0_14_4_%s\n' "$symbol" "$symbol"
done > "$scratch/symbols"
llvm-objcopy --redefine-syms="$scratch/symbols" \
    "$WITCHY_RUNTIME_RUST_ARCHIVE" "$scratch/runtime.a"
cc -O2 -Wall -Werror -Wl,--gc-sections -I "$AWS_LC_ROOT/include" \
    tests/native-linux/pkix-allocation-probe.c "$scratch/runtime.a" \
    "$AWS_LC_ROOT/lib/libaws_lc_fips_0_14_4_crypto.a" -lpthread -o "$scratch/pkix-probe"
"$scratch/pkix-probe"
```
The regular timing run used the default provider
configuration. A separate macOS ARM64 build linked the pinned AWS-LC
shared library and passed all 92 `wasm_cli` tests, including P-384,
PKIX, digest and signer tests. The linked library exported the pinned
symbols declared by the P-384, digest, signer and TLS adapters. The
native qualification runner separately passed `crypto/tls-aead.wy` in
default, checked, and observed modes with the pinned provider. It exercises
AES-128-GCM and AES-256-GCM record seal/open against known answers. The
TLS signer smoke fixture (`tests/fixtures/native-sandbox/tls-signer-abi.wy`)
also passed on macOS ARM64. It received a generated P-384 PKCS#8 key over
a launch descriptor and emitted an ECDSA signature. OpenSSL verified the
signature against the TLS 1.3 server CertificateVerify domain and the
fixture's 48-byte transcript hash. The fixture also checks that a
truncated DER key and a valid P-256 key both fail at launch without
emitting a signature. Other
provider failures and raw ABI pointer preconditions remain open.

Run the signer smoke test from the repository root with the provider-enabled
compiler and its qualification ready binding. Set `WITCHYC` and
`WITCHY_WASM_READY_BINDING` to their absolute paths.

```sh
(
    set -eu
    export WITCHY_WASM_READY_BINDING
    scratch=$(mktemp -d)
    trap 'rm -rf "$scratch"' EXIT
    cp witchyc/tests/fixtures/native-sandbox/tls-signer-abi.wy "$scratch/main.wy"
    openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:secp384r1 -out "$scratch/key.pem"
    openssl pkcs8 -topk8 -nocrypt -outform DER -in "$scratch/key.pem" -out "$scratch/key.der"
    openssl pkey -in "$scratch/key.pem" -pubout -out "$scratch/public.pem"
    "$WITCHYC" build "$scratch/main.wy" -o "$scratch/program" --target=macos-arm64
    "$scratch/program" --output --secret-slot=signer:sign/tls13/server-p384:fd:3 \
        3<"$scratch/key.der" >"$scratch/signature.der"
    python3 -c 'import sys; sys.stdout.buffer.write(b" " * 64 + b"TLS 1.3, server CertificateVerify\x00" + bytes([7]) * 48)' \
        >"$scratch/message"
    openssl dgst -sha384 -verify "$scratch/public.pem" \
        -signature "$scratch/signature.der" "$scratch/message"
    head -c 12 "$scratch/key.der" >"$scratch/truncated.der"
    openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:prime256v1 -out "$scratch/p256.pem"
    openssl pkcs8 -topk8 -nocrypt -outform DER -in "$scratch/p256.pem" -out "$scratch/p256.der"
    for invalid_key in "$scratch/truncated.der" "$scratch/p256.der"; do
        if "$scratch/program" --output --secret-slot=signer:sign/tls13/server-p384:fd:3 \
            3<"$invalid_key" >"$scratch/rejected.out" 2>"$scratch/rejected.err"; then
            exit 1
        fi
        test ! -s "$scratch/rejected.out"
        rg -Fx 'invalid or duplicate native secret slot binding' "$scratch/rejected.err"
    done
)
```

Kani verifies the size checks used by the digest ABI for every 64-bit
length and capacity when the digest mode is valid. An admitted output
fits the 64-byte scratch buffer, and an admitted input fits the provider's
length type without truncation. A separate harness calls the actual
digest entry point with null addresses for all rejected lengths and
capacities and proves that the call returns its expected error before
reaching the provider. Admitted nonempty spans require readable input
and writable output storage. The inlined checks
produced identical instructions in both default ARM64 runtime builds.

The offline `tests/fixtures/native-sandbox/digest-abi.rs` fixture passed
against CommonCrypto on macOS ARM64. It checks that SHA-256, SHA-384 and
SHA-512 produce the same output with shared input/output storage as with
separate buffers, leave the bytes beyond the digest unchanged, accept a
null address for an empty input, and reject invalid capacity or oversized
length before accessing either guest address. Build it with
`mbx exec rustc --edition=2024 -C opt-level=2 -C panic=abort` and run the
executable on macOS. Nonempty, admitted spans still require valid
caller-provided storage. For each digest width, the fixture also hashes
64 MiB of zeros, checks an independent SHA-2 known answer, and verifies
the bytes after the digest remain untouched.

On Linux ARM64 musl without a provider, the fixture also passed the
capacity and length error cases and received the provider-unavailable
result with null addresses. This verifies the early-return path in
the provider-free build. A separate Linux ARM64 VM run exercised the real
digest export against a scratch ELF build of pinned AWS-LC FIPS 0.14.4.
It checked all three SHA-2 widths against Python's hashlib, including
64 MiB inputs, output sentinels, empty input with a null address,
overlapping input and output, and early rejection of invalid spans.
The Rust runtime object targeted ARM64 musl with LSE atomics enabled
and was loaded through a small shared object in the glibc VM.
Witchy's Linux linker uses a static provider archive in a PIE. A second
Linux ARM64 VM run used the pinned FIPS static archive, including the
delocated object with its injected integrity hash, in a glibc PIE.
The P-384 fixture passed there as well.
The existing `tests/native-linux/signer-probe.c` passed in a separate
PIE using the same provider archive and the Rust runtime static library.
It reported FIPS mode enabled, matched the expected ACVP public-key
digest, produced a verifiable signature after installing the guest
sandbox, and rejected a message outside the slot's role.
An ML-DSA-65 probe in the same static-provider configuration signed an
empty message with AWS-LC and verified it through the runtime export
with a null message address. A corrupted signature failed. A short key,
short signature, and message longer than 64 MiB all returned false with
null addresses. The adapter checks lengths before constructing slices
and substitutes static storage for an empty message. It frees the
provider digest context before the public key, including after failed
initialization or verification. These observations cover the listed
ML-DSA-65 cases on Linux. The standalone
`tests/fixtures/native-sandbox/mldsa-abi.c` probe passed the same cases
for suite 65 on Linux ARM64 with the current Rust runtime archive and
the pinned static provider, and for suites 65 and 87 on macOS ARM64.
On Linux, use the signer probe's static link command in
`tests/native-linux/manifest-checks.sh` with the ML-DSA fixture in
place of `signer-probe.c`. The VM run cross-linked it as a PIE with
a scratch startup object. On macOS, build the runtime
dylib as for the digest fixture with `_witchy_public_mldsa_verify`
exported, then compile the probe with the pinned headers and link
against both the runtime dylib and the provider. Admitted nonempty spans still require readable
caller storage, and provider allocation failure was not injected here.
These fixtures used a scratch startup object in place of the normal
compiler-driven executable link.

The fixture also passed on macOS ARM64 with the pinned AWS-LC FIPS
provider. From `witchyc/`, set `AWS_LC_LIB` to the absolute path to
the provider's library directory and run:

```sh
scratch=$(mktemp -d)
mbx exec rustc --crate-type=lib --edition=2024 -C opt-level=2 -C panic=abort \
  --cfg witchy_native_aws_lc --emit=obj runtime/native/runtime_abi.rs -o "$scratch/runtime.o"
xcrun clang -arch arm64 -dynamiclib "$scratch/runtime.o" -L "$AWS_LC_LIB" \
  -laws_lc_fips_0_14_4_crypto -Wl,-rpath,"$AWS_LC_LIB" -Wl,-dead_strip \
  -Wl,-exported_symbol,_witchy_public_digest -o "$scratch/libwitchy_digest_runtime.dylib"
mbx exec rustc --edition=2024 -C opt-level=2 -C panic=abort --cfg witchy_native_aws_lc \
  -L "native=$scratch" -l dylib=witchy_digest_runtime \
  tests/fixtures/native-sandbox/digest-abi.rs -o "$scratch/digest-abi"
"$scratch/digest-abi"
```

The P-384 boundary fixture `tests/fixtures/native-sandbox/p384-abi.c`
passed on macOS ARM64 and in the Linux ARM64 scratch configuration
above with the pinned provider. It checks invalid
lengths before pointer access, unchanged outputs when an invalid peer
is rejected, and agreement between the one-shot and stateful APIs. It
also checks a sentinel immediately after each successful output span.
A failed stateful finish consumes its handle without changing the
secret buffer, and a later public-key request on that handle returns
an error without changing its output. Filling all 16 slots, releasing
one, and allocating its replacement also left the old handle invalid.
A separate Kani harness calls the same slot table with modeled key
values and proves sequential transfer out, reuse of the freed slot,
and rejection of the stale handle. A second proof starts at the last
two counter values and verifies that exhausting the handle space
prevents insertion even when a slot is free. The models exclude
simultaneous access and provider key allocation and destruction.
A separate Miri test uses the real slot guard with modeled keys and
two threads. It passed with strict provenance, Tree Borrows, and
forced preemption across four additional scheduling seeds. This
samples interleavings of insertion, lookup and removal.
To reproduce it, link the AWS-LC runtime object as above with only
`_witchy_p384_start`, `_witchy_p384_public`, `_witchy_p384_finish`,
`_witchy_p384_close`, and `_witchy_p384_agree` exported. Then compile the C fixture with
`xcrun clang -arch arm64 -std=c11 -Wall -Werror`, passing that dylib
as a link input, and run the executable.

The pinned provider source in `crypto/fipsmodule/cipher/aead.c`
checks the output capacity before seal and the default open path
forward to their cipher callbacks. On success it reports the produced
length. On error it zeros the passed output capacity and resets the
reported length. The selected TLS AES-GCM seal callback delegates to
the AES-GCM implementation in `crypto/fipsmodule/cipher/e_aes.c`.
`OPENSSL_cleanse` in `crypto/mem.c` writes zeros over the supplied range
without requiring its prior contents to be initialized. The Rust
adapter passes only its own scratch buffer, copies out only after
success with the exact expected length, and frees the provider context
before the copy. The cipher callback and assembly implementations
remain trusted for the bytes they actually write.

Kani checks the actual TLS record entry point with arbitrary mode and
length fields. All combinations rejected by the size, mode, or shape
checks return the expected code with null addresses. Provider imports
are left unresolved in the model, so a rejected request reaching them
fails verification. A second harness verifies early rejection for all
tag comparisons whose lengths differ from 48 bytes. The native
fixture checks that the provider compares exactly 48 bytes, including
a difference at byte 47 and ignoring a different byte at offset 48.

The standalone `tests/fixtures/native-sandbox/tls-record-abi.c`
fixture checks the real AES-128 and AES-256 adapters with the pinned
provider. Empty records accept a null address for the zero-length
span. Three-byte records round-trip with unchanged output sentinels,
and corrupted tags leave the entire output untouched. Wrong and
oversized lengths are rejected before memory access. The fixture
also seals and opens a 16,640-byte ciphertext at the adapter limit
and checks both output sentinels. An in-place AES-128 case exercises
overlap through the separate provider scratch buffer. On failure it
leaves the entire record intact, and on success it replaces only the
plaintext prefix. Use the same dylib link procedure as the P-384 fixture with
`_witchy_tls13_aead` and `_witchy_verify_tag48` exported.

The inventory covers the 54 files under `runtime/native/` with explicit
unsafe blocks, unsafe functions or foreign declarations, or global
assembly. The two additional files that use only unsafe symbol export
attributes are `broker_transport.rs` and `signer_binding.rs`. Their
exported names and calling conventions still form part of the runtime
ABI. The AWS-LC-enabled runtime also includes `src/compiler/pkix.rs`
directly. Its provider calls and ownership wrappers are in scope with
the cryptographic providers below.

| Boundary | Files in `runtime/native/` | Safety obligations |
|---|---|---|
| Heap, runtime entry, and global state | `global.rs`, `heap.rs`, `runtime_abi.rs` | Raw payload pointer provenance and lifetime, occupancy, handle generation, and exclusive process-state access. |
| Task contexts | `task_switch.rs`, `tasks.rs` | Live stack and context ownership across switches and reclamation, including abnormal termination. |
| Libc and platform setup | `sys.rs`, `errno.rs`, `linux_stat.rs`, `linux_sandbox.rs`, `seatbelt.rs`, `seatbelt_fds.rs`, `secret_guard.rs`, `deadline.rs`, `clock_random.rs` | FFI argument layout, initialized kernel outputs, signal behavior, and lifetime of process resources. |
| Launcher and grants | `launch_args.rs`, `launch_env.rs`, `launch_flags.rs`, `launch_plan.rs`, `launch_walk.rs`, `policy_grants_abi.rs` | Bounds and lifetimes for argv, environment and launch-plan buffers, plus grant ownership. |
| Broker, network, and timers | `broker_directory.rs`, `broker_endpoint.rs`, `broker_loop.rs`, `broker_socket.rs`, `connection_queue.rs`, `socket_primitives.rs`, `timers.rs` | Socket address lengths, descriptor ownership, and buffer and state lifetimes across broker operations. |
| File and console I/O | `console.rs`, `directory.rs`, `file_reader.rs`, `file_snapshot.rs`, `file_writer.rs`, `input_stream.rs`, `io_budget.rs` | Initialized counts from reads, writable output ranges, and file and descriptor ownership on cleanup. |
| Child processes and transport | `exec_broker.rs`, `exec_capture.rs`, `exec_grant.rs`, `exec_guest.rs`, `exec_linux_children.rs`, `exec_linux_limits.rs`, `exec_linux_runner.rs`, `exec_linux_security.rs`, `exec_linux_transport.rs`, `exec_pending.rs`, `exec_programs.rs`, `exec_supervision.rs`, `exec_temp.rs` | Child and descriptor lifetime across fork, wait, transfer, cancellation, and partial I/O. |
| Cryptographic providers | `digest.rs`, `p384.rs`, `p384_slots.rs`, `pkix_abi.rs`, `signer_crypto.rs`, `tls_record.rs`, `tls_signer.rs` | Provider-specific pointer ranges, initialization, ownership, and callback lifetime. |

The runtime's process-state cells (`runtime/native/global.rs`) have their own harness crate,
`tools/native-global-kani` (run the same way). It checks that a `Table` agrees with a plain array
and count over any six pushes and clears of four places, and that the pointer handed to C reaches
every item in use; and that a `State`, the cell for a value changed in place, keeps what a use
writes and stops the process on a second use inside a first. It does not check the claim the cells rest on: that the runtime has one thread.

`tools/native-logic-kani` checks pure logic the launcher, the supervisor and the guest's broker
client rest on (27 harnesses; proofs of a file's private functions sit in that file under
`cfg(kani)`). Each holds for every input up to the bound given, and each `kani::cover!` in it is
satisfied:

- launch plan: a line splits into one to eight fields that are the whole line, with no empty field
  and no control byte (lines to 17 bytes); a record's width fits a line; an inherited descriptor's
  number is 3 to 1023 and is its one to four digits (6 bytes); an object identity is digits, a
  colon, digits, at most twenty a side (43 bytes).
- launch arguments: the stated limits are at most 256 words and 65536 bytes (12 bytes); one limit
  is its digits' value and no more than its bound, for every bound (11 bytes); the runtime's UTF-8
  check agrees with `core::str::from_utf8` (5 bytes); a limit label belongs to a known flag and is
  1 to 48 plain bytes after the first `=` (61 bytes).
- connection queue: for any ring of up to 4096 places, a push is taken exactly when there is room
  and lands after the last descriptor waiting, a pop takes the head, both keep the indices inside
  the ring, and two pushes onto an empty queue pop in order; the listener's address is written
  whole or not at all, with the port's digits and brackets round a bare IPv6 host (host text to 5
  bytes, buffer to 12, every port).
- broker: a reply's payload is never longer than the request asked for, for every header; a DNS
  endpoint that is accepted is a host of letters, digits, dots and inner hyphens, a colon and a
  port that is not zero (10 bytes); a staged file's name is digits, a hyphen, digits (43 bytes); a
  directory path's length stays inside its 4096-byte array for every push.
- Exec: a command's deadline never passes the launch's; accepted argument offsets are exactly
  `count` zero-ended texts that are the whole buffer (7 bytes); a swept directory's name gives a
  process number from 2 to 999,999,999 (35 bytes).
- timers, on a table cut to eight places for the proof (`SLOTS` under `cfg(kani)`): a handle finds
  only the timer that holds it; a new timer takes the first free place from `next`, changes no
  other, and is found by its handle.
- an accepted environment name is plain; a file identity is two decimal numbers; a budget debit
  matches its policy (the harness `budget.rs` carries).

They bound input length, and say nothing of the code that calls these functions. The crate
includes most of the runtime's files to reach these functions, so a file that gains a dependency
on one not listed in its `lib.rs` stops the crate from building until that file is added. The
whole run takes about twelve minutes; the slowest harness takes under three.

`tools/native-sandbox-kani` (run the same way; `linux_sandbox.rs` and macOS's `seatbelt.rs` export
the same symbol, so it has its own crate) checks the three seccomp programs the Linux runtime
builds, at the point where it would install them: for every grant and every descriptor, the
program is at most 256 instructions, ends by returning, and every jump lands inside it; a rule
made by the builder's `syscall` helper has a body no longer than its one-byte skip. It does not
check what the programs allow, or that a hand-counted skip lands on the start of the next rule.
The endpoint and argument harnesses bound their input to ten and seven bytes, so they do not
reach the length limits those functions also apply (63-byte labels, 253-byte hosts, 4096-byte
arguments).

Three additional harnesses were checked individually: one-byte fills of up to
eight cells, two-byte fills of up to four cells, and transfers of up to four
cells. They exercise the actual pointer and occupancy operations, including a
fill source inside the destination allocation. The models use valid allocated
buffers and do not prove arbitrary pointers, larger buffers, or the compiler's
calling convention. Miri also checks an overlapping fill using an allocation
with real pointer provenance. Its allocator test also runs real ABI calls
through a small independent word model: allocation, initialization, transfer,
read, refused release, cell removal and release across 25 range combinations.
These are targeted checks, not evidence that all runtime unsafe code is sound.

A separate Kani harness calls the actual word initialization and read ABI
with a valid preallocated eight-byte sparse buffer and a four-entry slot
table. For every initial payload and every valid offset and count in that
buffer, it checks byte order, preservation of bytes outside the range,
round-trip readback, and the resulting bitmap and occupancy count. The
harness supplies the allocation and exercises one live slot.

The slice-initialization ABI is checked with four two-byte elements and
every valid pair of source and destination ranges in that allocation.
The expected bytes come from the original payload, so overlap in either
direction must preserve copy semantics. The proof also checks the
untouched payload, bitmap, occupancy count, and an empty copy with a
null source. Nonempty sources are readable by construction in this
harness. The caller's ability to supply such a source remains an ABI
precondition. A separate Miri-only fixture uses an actual allocated
buffer and passes an overlapping pointer derived from that allocation
to the same ABI. It also passes a null source for a zero-length copy.
Both the fill fixture and this slice fixture passed under
`MIRIFLAGS=-Zmiri-strict-provenance`.

Native allocation returns one word. Successful handles carry a nonzero
generation in the upper 32 bits. Allocation errors have a zero generation.
Both buffer and box lowering branch on the generation, so the allocator no
longer writes through a pointer supplied by generated code. Box lowering keeps
the original value on the failure branch.

## Integer SIMD and byte hashing

`import simd` provides four unsigned 128-bit vectors, `U8x16`, `U16x8`,
`U32x4`, and `U64x2`. The two fields `low` and `high` hold packed words.
Lane zero occupies the least significant bits of `low`. Use `from_lanes` to
construct a vector from individual values, or `splat` to broadcast one value.
The ARM64 executable uses Cranelift vector instructions and the Wasm backend
emits SIMD128. Arithmetic wraps within each lane. Some operations require more
than one instruction, including 64-bit lane multiplication on ARM64.

`cmp_eq`, `cmp_gt`, and `cmp_lt` produce all-ones or all-zero lanes. For
byte vectors, `group.eq_mask(U8x16.splat(tag))` compares the lanes and
returns a U64 whose low 16 bits identify matching byte positions. `select` selects bits from two
vectors. Other operations include wrapping add/sub/mul, bitwise operations,
unsigned min/max, lane lookup, reversal, and wrapping horizontal sum.
`load` and `store` use a slice of the lane type and check the entire range.
`load_words` and `store_words` use two U64 elements.

`import word` provides scalar `a.widening_mul(b)`, returning both U64 halves
of the product. `import foldhash` provides `foldhash(bytes: &[U8])`. It uses
the existing checked `read_le64` byte-slice operation and matches a single
64-bit little-endian upstream fast write with per-hasher seed `U64::MAX` and
the fixed shared seed. Run `scripts/bench-foldhash.py` with Python and the path
to a built compiler to benchmark the executable.

For a standalone vector-ABI object, use
`witchyc build-object examples/native-simd-object.wy -o add.o`. This entry
point accepts functions with same-shape vector parameters and result, and a
single return expression using binary vector operations. It uses host Clang on
ARM64 Advanced SIMD and x86-64 SSE2. Full native guest executables use the
targets documented above.

### Packed binary64

`import simd` exposes `F64x2(low, high)`, `F64x2.splat(value)`, and the
`low` and `high` fields. `add`, `mul`, and `div` apply the corresponding
binary64 operation independently to each lane. They use the same rounding
and exceptional-value rules as scalar `F64`, including signed zero and
division by zero. NaN payloads are unspecified. There is no reassociation
or fused multiply-add in these operations.

On ARM64 native targets the backend emits packed operations. The public
methods use two scalar operations on Wasm, so they work with the scalar
Wasmtime profile. Like the integer SIMD records, these values currently
use a two-word language representation. Keeping them in vector registers
across operations is an optimization opportunity.

`benchmarks/witchy/parts/spectral-paired.wy` is a paired-row experiment.
Each lane accumulates one output row in the original summation order.
The default spectral benchmark keeps the scalar implementation.

In a macOS ARM64 comparison at n=1000, both programs printed
`1.274224148`. An instrumented build compared all 2,000 final vector
elements bit for bit. The Wasmtime fallback printed the same result.
ARM64 disassembly shows `fdiv.2d`, `fmul.2d`, and `fadd.2d`, each
emitted twice in the paired loop as it computes the two result words.

Seven alternating samples after warm-up gave these launch-to-exit
medians. Runtime ranges were wide on the shared host.

| Variant | Cached-proof build | Runtime | Runtime range |
| --- | ---: | ---: | ---: |
| Scalar | 162.1 ms | 118.9 ms | 89.2–225.0 ms |
| Paired | 164.2 ms | 1569.6 ms | 824.2–2246.4 ms |

The paired loop also uses checked index and denominator arithmetic,
while the scalar version proves some of those checks redundant. This
comparison measures the complete implementations, including those
differences and the cost of packing and extracting lanes.

The two-result packed primitive replaces the original pair of single-lane
intrinsics. It receives four binary64 operands and emits one vector
instruction before extracting its two results. The same primitive emits two
scalar operations on Wasm. ARM64 disassembly of the paired benchmark now
shows one packed divide, multiply and add each. A fresh interleaved
comparison (nine measured runs per variant) took 137.3 ms for scalar,
938.0 ms for the original paired variant, and 847.0 ms for the new one.
The original and new paired runtime ranges overlapped on the shared host.
This change preserves the existing two-word layout and calling convention.

#### Register-resident floating-point pairs

Native lowering now carries a record with exactly two binary64 fields in
one vector SSA value when the local has no address. Internal function
parameters and returns of that shape use a vector carrier. Addressed values,
arrays, and enclosing aggregates still use their declared memory layout.
When an operation consumes the two matching projections of one local, it
uses the existing vector value. Equal SSA operands use one broadcast.

At n=1000, the scalar, paired-scalar and paired-SIMD builds printed
`1.274224148`. A separate instrumented run compared all 2,000 final
vector elements bit for bit across the three versions. The paired Wasm
build uses scalar arithmetic and printed the same result in Wasmtime.
To reproduce the native control, change the `static if` backend comparison
in `parts/spectral-paired.wy` from `.Native` to `.Wasm` in a temporary copy.
The two paired variants share denominator arithmetic, input reads, loop
checks, and output stores. The original scalar benchmark uses its original
proved per-row computation.

| macOS ARM64 variant | Cached-proof build median | Runtime median | Runtime range |
| --- | ---: | ---: | ---: |
| Original scalar | 129.1 ms | 61.7 ms | 56.7–79.9 ms |
| Paired scalar | 132.0 ms | 59.2 ms | 50.6–62.8 ms |
| Paired SIMD | 138.9 ms | 53.6 ms | 44.1–65.1 ms |

Seven build and eleven runtime samples per variant, rotating run order after
warm-up. Runtime is launch to exit. The shared host's runtime ranges overlap.

Disassembly shows one `fdiv.2d`, one `fmul.2d`, and one `fadd.2d` in the
hot loop. The last updates `v4` in place, with no helper call, accumulator
spill, or lane extraction before the backedge. The denominators still need
two integer-to-float conversions and one pack, and the shared input needs a
broadcast. Remaining scalar work includes overflow branches, constant
integer division lowering, and reloads of the slice header. The two lanes
are extracted once outside the inner loop for the checked output writes.

#### Proved arithmetic and scaled loads

The native backend now uses the same admitted arithmetic coverage and
counted-step facts as the Wasm emitter for unsigned additions,
subtractions, and multiplications. Other arithmetic retains its checks.
Slice headers without an address stay in SSA, and power-of-two strides
are expressed as shifts that ARM64 can fold into an indexed load.

The paired spectral benchmark computes denominators by recurrence. It
selects the transpose-dependent offsets outside the inner loop and
proves the recurrence stays in range. At n=999 and n=1000 all final
vector elements matched the scalar benchmark bit for bit in both paired
modes. Wasmtime also matched the expected output at n=1000. A false range
invariant was refused and an unproved overflowing addition still trapped.

The native SIMD inner loop has a single scaled input load, two
integer-to-float conversions, and one packed divide, multiply, and add.
It has no denominator overflow branches, transpose branch, helper
calls, or accumulator spills. The denominator pack, input broadcast,
and scalar recurrence updates remain.

| macOS ARM64 variant, n=1000 | Cached-proof build median | Runtime median | Runtime range |
| --- | ---: | ---: | ---: |
| Paired SIMD before the lowering audit | — | 49.01 ms | 43.34–52.18 ms |
| Original scalar with current compiler | 109.47 ms | 49.18 ms | 44.42–56.60 ms |
| Matched paired scalar | 116.90 ms | 55.31 ms | 50.98–59.59 ms |
| Paired SIMD | 110.44 ms | 34.19 ms | 28.18–35.73 ms |

Seven build samples and eleven launch-to-exit runtime samples per variant,
rotating order after warm-up. The before/after row includes both native
lowering changes and the proved denominator recurrence.

#### General lowering and bulk byte paths

Unsigned division by a constant nonzero divisor uses an exact scaled reciprocal
or a shift. Remainder shares the division quotient when both use the same SSA
dividend in one block. Indexed address calculations are likewise reusable
within a block. The bounds decisions and memory loads remain separate from
address calculation. Float expressions load into floating-point registers.

Two-word native results return directly, and unaddressed records with two
scalar fields can live in SSA. The inliner accepts small call-bearing
wrappers and charges their original frames and instruction counts. The
benchmark programs also use the library's borrowed byte output and append
paths. These library calls keep the usual capability and allocator checks.

When a direct call exposes a stack-backed argument or an indirect result,
the inliner can spend up to 16 additional instructions on the callee within
the 512-instruction caller budget. After such an inline, the
compiler forwards exact stack stores to loads along linear control flow
and removes stores from slots with no remaining readers or escaping
addresses. These passes run after proof admission. Calls and unresolved
writes invalidate the forwarding state. The pass keeps runtime memory
checks and does not treat heap accesses as stack accesses.

For local A/B builds, `WITCHY_NATIVE_INLINING=off` disables inlining and
`WITCHY_NATIVE_INLINING=baseline` selects the earlier small-body policy.
The default and `selective` select the aggregate-aware policy. In an
ARM64 diagnostic with ten million calls updating a three-field record,
the baseline and selective policies took 64.5 and 23.4 ms. Warm build
medians were 68.7 and 68.4 ms, and both binaries were 191,224 bytes.
Mandelbrot and fannkuch emitted identical assembly under both policies.
Paired spectral norm grew by 32 bytes and showed no repeatable runtime
improvement.

The following macOS ARM64 medians compare `8d2fa729` with `3de97fca`.
The runtime comparison includes the source edits in FASTA and string.
Five cached-proof builds and nine launch-to-exit runs per variant were
measured in alternating order after warm-up. All nine pairs matched stdout.

| Benchmark | Build before / after (ms) | Runtime before / after (ms) |
| --- | ---: | ---: |
| FASTA | 118.62 / 125.35 | 86.13 / 80.21 |
| Mandelbrot | 113.90 / 116.81 | 256.62 / 211.74 |
| N-body | 163.64 / 190.95 | 111.69 / 94.32 |
| Spectral norm | 174.06 / 159.80 | 34.69 / 36.03 |
| Sort | 304.50 / 276.86 | 118.84 / 122.72 |
| Map | 87.83 / 87.94 | 86.43 / 68.85 |
| Trees | 85.05 / 84.79 | 130.03 / 137.45 |
| String | 105.11 / 117.31 | 96.55 / 60.17 |
| Words | 159.34 / 142.92 | 123.13 / 93.44 |

Repeated sort, trees, and spectral measurements had overlapping ranges.
Trees reversed direction in the repeat, and sort's before/after direction
changed in a seven-variant follow-up. Treat these results as mixed.

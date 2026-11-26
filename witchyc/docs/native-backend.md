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
RUSTUP_TOOLCHAIN=nightly mbx miri test --offline \
  --manifest-path tools/native-core-miri/Cargo.toml --lib -- --test-threads=1
```

Miri runs these core tests in its interpreter. Native guest code, the OS adapters
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

All 13 harnesses passed on macOS ARM64. Occupancy checks cover up to three
operations on eight cells, and the multi-slot check uses two slots. These checks
cover the individual core functions and the states in each harness. The read
check uses an eight-byte buffer and replaces the hardware trap with a Kani
failure. The OS adapters and generated machine code are outside their scope.

The runtime's process-state cells (`runtime/native/global.rs`) have their own harness crate,
`tools/native-global-kani` (run the same way). It checks that a `Table` agrees with a plain array
and count over any six pushes and clears of four places, and that the pointer handed to C reaches
every item in use; and that a `State`, the cell for a value changed in place, keeps what a use
writes and stops the process on a second use inside a first. It does not check the claim the cells rest on: that the runtime has one thread.

`tools/native-logic-kani` checks pure logic the launcher, the supervisor and the guest's broker
client rest on (24 harnesses; proofs of a file's private functions sit in that file under
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
the existing 128-instruction caller budget. After such an inline, the
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

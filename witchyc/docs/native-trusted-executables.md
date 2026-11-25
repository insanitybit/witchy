# Native trusted executable manifests

Status: implementation spec, launch plan version 1 (macOS ARM64). Decided 2026-11-23; see
[decisions](decisions.md#one-grant-document-and-an-embedded-launch-plan-2026-11-23).

A native Witchy executable can carry the grant policy chosen by its binary author. When the
user runs it, the trusted runtime resolves values such as paths and descriptors according to
that policy and constructs every capability before Witchy code runs. A library receives
authority only through the capability values the program passes to it.

## What it looks like

```rust
fn main(out: Console<Write>):
    out.print_int(42)
```

```toml
[launch]
version = 1
```

```sh
witchyc build hello.wy -o hello --target=macos-arm64 --manifest=hello.grants.toml
./hello
```

`Console<Write>` and `Allocator` need no entry, as in a `wasm-grants` document. A tool that
reads an operator-selected project:

```toml
[launch]
version = 1

[flags]
project = { type = "path", required = true }

[dirs]
project = { root = { from = "arg", name = "project" }, rights = ["Read"] }
```

`./notes-tool --project ./notes` (or `--project=./notes`) passes a read-only `Dir` to
`main(project: Dir<Read>, out: Console<Write>)`. The release executable runs without
Grimoire or the TOML file. `witchyc native-plan PROGRAM MANIFEST` prints the plan a build
would embed, with each source and limit and no resolved value.

## One document

The manifest is a [grant document](../src/compiler/grant_document.rs): the same parser, sections
and parameter mapping that `wasm-grants` reads (`[dirs]`, `[files]`, `[input]`, `[env]`,
`[args]`, `[net]`, `[listen]`, `[clock]`, `[rand]`, `[secrets]`, `[user_caps]`, `[paths]`).
A manifest adds:

- `[launch] version = 1`, required, the only entry of that section.
- `[flags]`, the declared launch arguments: `NAME = { type = "path" | "fd", required = true,
  secret = BOOL }`. The flag is `--NAME`. Every declared argument is required (an optional one
  would need a default, which version 1 does not have). `secret = true` is a descriptor that
  feeds a secret. A name of a development launch flag (`output`, `dir`, `secret-slot`, ...)
  is refused.
- Typed sources wherever a path or secret source is written. A string is a literal.

| Source | Meaning | Taken by |
| --- | --- | --- |
| `"..."` or `{ from = "literal", value = "..." }` | Fixed by the binary author, absolute | dir, file, input, `within` |
| `{ from = "arg", name = "project" }` | A declared argument | dir, file, input (path or fd argument), secrets (secret fd argument) |
| `{ from = "home", path = "..." }` | Under the launching account's home, from the account database | dir, file, input, `within` |
| `{ from = "cwd", path = "..." }` | Under the startup directory, captured once | dir, file, input, `within` |
| `{ from = "env", name = "NAME" }` (or `"env:NAME"`) | A variable, taken out of the environment | `Secret<Seal>`, `Secret<Reveal>` |
| `{ from = "fd", number = 3 }` (or `"fd:3"`) | One inherited descriptor, checked against the grant's kind | dir, file, input, secrets |

`home` and `cwd` paths are relative and plain: no `.`, `..`, empty component, or control
character. A directory entry takes `root`, `rights` (which must be the type's), `policy =
"data-only"` and `within`. A file or input is a source, `{ path = SOURCE, within = SOURCE }`,
or for input `{ stdin = true }`. `within` restricts a path argument only, and is a literal,
home or cwd source: the argument cannot choose its own boundary. `[paths] links = false` is
accepted and changes nothing: a native launch never follows a link.

`wasm-grants` accepts the same document when every source is a literal, and refuses a
declared argument or a runtime source, which only a manifest-bound native executable
resolves.

## Compiler checks

Grimoire hashes the native launch document as it reads it. The standalone
`native-plan` and `build` commands accept `--manifest-digest=SHA256` to
check that document against the digest. Supply 64 lowercase hexadecimal
characters and include `--manifest=PATH` on the build. Grimoire's
resident build sends the same digest through its compiler session.
A changed launch document is refused before the executable can replace
the output.

`--manifest=PATH` is given once, on a native `build` or `build-session`. The compiler
reads the manifest once (a regular UTF-8 file of at most 64 KiB) and checks it against the
checked `main`, before proof work:

- Each parameter maps to its section by name, kind and rights. A missing grant, a stated right
  the type lacks, an entry no parameter takes, an unknown section, source or field, and a
  declared argument no source takes are errors. An argument reaches one grant.
- `[exec] NAME = { dir = SOURCE, programs = ["name"] }`: 1 to 16 plain names, fixed in the
  plan; the directory is a literal, home, cwd or descriptor source, never an argument, so no
  argument can choose or add a program.
- `File<Write>`: a path source, or `{ path = SOURCE, mode = "replace" | "create", within =
  SOURCE }`. The directory part is resolved as a directory source is; the leaf is a plain
  name. `replace` (the default) makes or replaces the file whole when it is closed; `create`
  makes it only if absent. A descriptor cannot name a written file. Combinations the runtime cannot keep apart are refused: two parameters of one
  kind (other than secrets and user capabilities), any directory pair other than one
  `Dir<Read>` with one `Dir<Write>`, data-only with two directories, and native secrets with
  `Exec`. A secret beside a `File<Read>`, `Console<Read>`, a directory of any rights or a written
  file is allowed (see "Secrets beside file grants"); the runtime refuses the launch when that
  file or input is a secret's source or declared object (device and inode), and stops the run
  when a directory open reaches one. One secret source reaches one slot, and a secret's variable
  is not also `[env]` or a store's secret.
  A `TlsSigner` loads a P-384 PKCS#8 key from a descriptor and can sign
  TLS server CertificateVerify transcripts. Its launch also excludes
  writable files and directories.
- `[launch]` siblings: `[limits]` carries `deadline-ms`, `output-bytes`, `input-bytes`,
  `fs-bytes`, `fs-entries`, `net-bytes`, `exec-bytes`, and for a socket-entry server
  `connections`, `waiting-connections`, `connection-deadline-ms`, `net-bytes-per-connection`,
  `output-bytes-per-connection`, `fs-bytes-per-connection` and `fs-entries-per-connection`. Each applies only to a grant `main` takes, is shown by
  `native-plan`, and is enforced exactly as the development flag of that name; it is not a
  launch argument.
- `identity = { from = "arg", name = ID }` on a directory, a read file or an input binds the
  opened root to a launcher's check: `ID` is a declared `type = "object"` argument, and the
  launch refuses unless the object it opened (as the plan says, without links) has that
  `DEVICE:INODE`. A launcher such as Grimoire checks a root's place, then passes its identity.
- An optional argument is `required = false` with `default = SOURCE` (a fixed absolute path,
  home or cwd source); when it is absent, its grant is resolved from that source under the same
  `within`. Only path arguments have defaults: a missing descriptor refuses the launch.
- `main(connection: Socket, ...)` is `[listen] connection = "HOST:PORT"`: one sandboxed worker
  per connection, with directory grants, `File<Read>`, `Console<Read>`, `Console<Write>`, `Net`,
  `Exec`, Allocator, Clock, Rand, Args, Env and user capabilities beside it. Each worker is
  forked from a supervisor that never runs guest code, with a broker of its own forked from the
  same supervisor, so nothing one connection did is in the next one's memory or descriptors:
  - `File<Read>` is read whole at launch (run-wide `fs-bytes` pays for it once); every worker
    reads its own copy from the start.
  - `Console<Read>` is read to its end once, before the first connection, and needs
    `[limits] input-bytes` of at most 16 MiB (refused otherwise, at build and launch); every
    worker reads the same bytes from the start.
  - `Net`: the endpoints are resolved once at launch, and each connection's broker dials only
    them, with the run-wide and per-connection network budgets.
  - `Exec`: the programs are opened once at launch (identities captured then); each broker
    runs copies of them, against an output budget shared by the run. A worker stopped mid-run
    has its broker stop the program (session, group, tree) before it goes.
  - The run-wide file byte and entry budgets are shared by all workers;
    `fs-bytes-per-connection` and `fs-entries-per-connection` give each connection its own beside
    them (a read file's bytes count against the connection's). A worker holds no root or program
    descriptor; only its broker does.
  - A `TlsSigner` is available to each native socket worker. Its private
    key is loaded at launch. The worker uses it through the fixed TLS
    signing operation. Other secret grants, `Listener`, and
    `File<Write>`/`File` are refused beside a `Socket`.
- Source-only fields (comments, review text, the document's path) are stripped. What remains is
  the plan, embedded as `witchy_launch_plan`: read-only data, tab-separated records, one per
  line, beginning `witchy-launch-plan\t1\tMANIFEST_SHA256` and ending `end`. A development build
  embeds a single NUL there.

The manifest's bytes are build input. The compiler reads them again before publication and
refuses to publish if they changed, leaving the prior executable intact; the manifest is never
the output. `build-session FILE -o OUT --target=macos-arm64 --manifest=PATH` takes
`build REQUEST_ID SOURCE_SHA256 MANIFEST_SHA256`, and a request whose manifest digest is not the
file's is refused with the prior output intact. The resident compiler (`compiler-session`)
takes `build-native ENTRY OUTPUT MANIFEST INPUT_KEY MANIFEST_SHA256`: `INPUT_KEY` is the
`input-key` answer for the entry, checked against the snapshot the build reads and again
before publication, and the manifest digest as above. The plan carries the manifest's digest,
so the executable names the policy it was checked against.

## Launch

A manifest-bound executable has no development flag parser. Startup:

1. Reads the embedded plan with no tolerance (unknown record, field count, missing end) and
   requires the grant kinds it names to equal the compiled entry's.
2. Binds the command line: each argument must be a declared `--NAME VALUE` or `--NAME=VALUE`,
   once, of its declared type (a path of 1 to 4096 bytes; a descriptor number 3 to 1023).
   Anything else, including any development grant flag, refuses the launch. Words after `--`
   are the `Args` words, only when `main` takes `Args`. Every required argument must be
   present. Nothing has been opened yet.
3. Resolves each grant in plan order. Paths are walked one component at a time from a
   directory descriptor (`/`, the captured startup directory, or the home directory) with
   `O_NOFOLLOW_ANY`, so no component is a link. With `within`, the boundary is opened first;
   the walk refuses `..`, must pass through the boundary (or start below it, found by walking
   `..` from the base descriptor), and every directory after it must be on the boundary's
   device. A restricted file must have one link. Home is the account database entry for the
   real user ID (`getpwuid_r`), never `$HOME`. An inherited descriptor is one open before the
   launch opened anything (recorded after step 2), so a descriptor source never names the
   startup directory or a root the launch opened itself. It must be the grant's kind
   (a directory, a regular file, or for input also a FIFO) and read-only for a file or input.
4. Hands the opened directory descriptors to the broker (the broker keeps only them and its
   channel), snapshots files from their descriptors, places input on standard input, and binds
   secrets through the existing slot binder (a secret argument supplies only the descriptor
   number).
5. Runs the shared startup checks and installs the sandbox, then enters Witchy code.

Any refusal exits with status 2 before entry; the kernel closes what was opened, and the
broker, if started, is reaped. Diagnostics name the argument and the reason, never a value.

A directory's rights are the type's: a `Dir<Read>` broker has no writable root, the runtime
traps a write operation on a read handle, and the guest's Seatbelt profile denies file opens.
`data-only` is the existing host policy: plain ASCII names, no dot components, protected names
kept from writes.

An explicitly unrestricted path argument lets the operator name any directory they can open,
and the reviewed plan says "unrestricted". Embedding the plan binds the policy to the
executable and creates no new OS privilege. Installation and distribution still determine
which executable the user trusts.

## Development builds

A native build without `--manifest` keeps the raw launch flags (`--output`, `--dir=PATH`, ...)
for compiler development and tests. The two cannot be confused: a manifest-bound executable
refuses every flag it did not declare, and a declared argument never has a development flag's
name.

## Secrets beside file grants

A secret's objects are the files its bytes came from: the regular file behind a secret's
descriptor source, and any declared `object` (`DEVICE:INODE`). Beside them a run may hold any
directory grant and a written file. What keeps the secret's bytes out of the guest, and the
guest's bytes out of the secret, is checked on descriptors, never on names. Each operation:

- *Open to read* (a directory's file, a listing's directory, data-only paths): the broker opens,
  `fstat`s the opened descriptor and compares device and inode with every secret's object
  before a byte is read. A match stops the broker, and the guest stops the run.
- *Open to append*: the same, on the descriptor opened `O_APPEND`, before a byte is written.
- *Create only* (`open_new`, `mode = "create"`): the bytes go to a new file made `O_EXCL` in the
  staging directory and published with `RENAME_EXCL`: a file that did not exist is no secret's.
- *Replace* (`open_file` on a writable directory, a written file's `replace`): the bytes go to a
  new file, and a rename moves the name onto it. The secret's inode is never opened for
  writing; at most a name stops naming it.
- *A writable root holding a secret's object* at any depth (so also any directory on the way to
  it) is refused at launch: the runtime walks the root on opened descriptors, into every device
  and through no link, and refuses when it finds one or cannot finish (65536 entries, depth 64).
  A plan names no secret's path (its sources are descriptors and variables), so this is a launch
  check. The exec directory and its programs are kept out of every writable grant the same way
  (identity overlap check).
- *Rename* (either side) and *remove*, in every mode: the broker walks the tree being moved,
  replaced or dropped on opened descriptors and refuses (`NotGranted`) when any entry is a
  secret's object or a granted program, or the walk cannot finish. This covers a secret moved
  into the root by another process after launch; a rename of a directory holding it is refused.
  They read and write no file's bytes.
- A *racing process* may swap names between any two of the broker's calls. It changes which
  object an open reaches, never which object is checked: the check is on the descriptor that
  will be read or written. Tested by swapping a key's and a decoy's names while the guest
  appends to and reads the decoy's name: the key's inode is never read or written, and every
  open that reached it stopped the run.
- Names: a replace, rename (either side) or remove whose name is a secret's object when the
  broker looks at it is refused (`NotGranted`), so the guest cannot drop or replace the
  launcher's key file's name (and choose the key its next run reads). The broker serves one
  request at a time, so only another process can come between that look and the change, and a
  process that can write the directory could replace the key file itself. This protects the
  name; confidentiality does not rest on it.
- *Exec* stays refused beside a secret: a program runs unsandboxed with the user's authority
  and could reach a secret's file by a name, or this process, which no descriptor check
  follows. Its children never get a secret's bytes or a descriptor because none run.

## Not in version 1

- A secret's `object = { from = "arg", name = "..." }`, of an argument of `type = "object"`
  (`DEVICE:INODE`), names the file a launcher read the secret from when it passes the bytes on
  a pipe (`--secret-object=DEVICE:INODE` in a development launch). It only restricts: no file,
  input or directory grant may reach that object during the run.
- `within` is checked once, at launch, on the opened descriptors. It bounds an operator who
  cannot write inside the boundary: one who can may move an outside directory into it first.
  Only an unrestricted path argument may use `..`; a regular file is checked before it is
  opened, so no device node is opened to be refused.
- `within` bounds the root object. Inside a granted directory, the broker refuses links and
  `..`, and `data-only` also refuses other devices.
- A regular file with more than one link is refused on every native route to a file, in both
  launch modes: a directory's open, replacement, append and rename (as source or target), a
  `File<Read>`, a named or descriptor input, and a regular file on standard input. A second
  name may be a file outside the grant. No grant opts out.
- Linux ARM64 supports native secrets when the compiler is built with the pinned AWS-LC FIPS
  archive. See "Linux: what is done".

## Linux: what is done

Checked inside a local Linux 6.8 aarch64 container (Docker's default seccomp profile, no network;
tests/native-linux/manifest-checks.sh, with `WITCHY_RUNTIME_RUST_ARCHIVE` and AWS-LC mounted, all 30
checks pass on the Rust runtime as they did on the earlier C one): the milestone binaries (hello, notes-tool),
Dir<Read> and Dir<Write> with links, `..` and hard links refused, Exec, an orphaned `setsid`
descendant stopped, and a socket-entry server with a read file, input, outbound Net, Exec and a
writable directory.

The Rust port also has focused Linux checks (tests/wasm_cli/native_linux.rs) for descriptor-bound
directory reads, network and Exec brokering, socket-entry execution, mount-boundary refusal, process
cleanup, and an embedded secret grant signing with the pinned AWS-LC library.

- **Plan and arguments**: the same Rust reader and binder as macOS; inherited
  descriptors are closed with `close_range` around every kept one.
- **Paths**: every link-free open is `openat2` with `RESOLVE_NO_SYMLINKS |
  RESOLVE_NO_MAGICLINKS`, and with `RESOLVE_BENEATH | RESOLVE_NO_XDEV` for a path relative to a
  granted directory. Stricter than macOS: a mount inside a granted directory is not
  crossed. `renameatx_np` is `renameat2` (`RENAME_NOREPLACE`, `RENAME_EXCHANGE`).
- **Guest**: Landlock grants it nothing; seccomp allows its output, its input, its broker
  channel (by descriptor number), private non-executable memory (the runtime's own buffers, as on
  macOS where the runtime checks Allocator), time, randomness and exit.
- **Broker** (the shared Rust request decoder with Linux confinement): after its setup it
  confines itself. Landlock (ABI 3 at least, 4 for network): every file-system right ABI 4
  handles is handled; a read root is granted `READ_FILE | READ_DIR`, a writable root and a written
  file's directory also `WRITE_FILE | MAKE_REG | MAKE_DIR | REMOVE_FILE | REMOVE_DIR | REFER |
  TRUNCATE`; TCP bind and connect are handled and connect is granted to the planned ports only.
  Not covered by ABI 4: device `ioctl` (ABI 5), which the broker's seccomp denies with every other
  call outside its allow-list (EPERM, never a kill). Hard-link, program and secret identity checks
  are the macOS ones, on the opened descriptor.
- **Exec**: programs cannot run inside the broker's Landlock domain (it is inherited, and a
  program needs the user's system files), so the broker forks a runner before confining itself.
  The runner holds only its channel to the broker and the opened programs, receives the broker's
  Exec requests unchanged, and runs programs as on macOS (copy, no environment, `/dev/null`,
  own session). No namespace and no privilege is used (user namespaces are not relied on:
  Docker's default seccomp profile and Ubuntu's `apparmor_restrict_unprivileged_userns` refuse
  them). The runner is a child subreaper (`PR_SET_CHILD_SUBREAPER`) and dies with the broker
  (`PR_SET_PDEATHSIG`): a descendant that starts its own session and whose parent exits is
  re-parented to the runner, so nothing a program starts leaves the runner's descendants.
  Stopping a program walks that tree top down from `/proc` (the runner's children, then each
  child's children from every thread's list), opens each process as a pidfd and checks it is
  still the child of the parent it was listed under (already stopped, so it forks nothing, and a
  re-parented process goes to the runner), sends it `SIGSTOP`, then sends every held process
  `SIGKILL` through its pidfd and reaps the runner's children without blocking (only those held
  under the runner, never the program, which the caller reaps). This repeats until the runner has
  no child but the program. Nothing is signalled by number, by group, or outside the runner's
  descendants (no `kill(0)`).
  Layers, each a child subreaper that stops what the one below leaves: the program runs with
  `PR_SET_PDEATHSIG(SIGKILL)` toward the runner (checked against a pipe whose only writer is the
  runner, so a runner already gone is seen); the runner's parent is the warden, a process the
  broker forks before confining itself, unconfined, holding only a pipe from the broker and its
  own children list: a child subreaper that, when the runner dies (a program of the same user can
  kill it) or its sweep passes 5 s, or the broker is done or gone, stops its own direct children
  (each unreaped, so its number is the warden's) through pidfds, SIGSTOP then SIGKILL, bounded by
  5 s, and exits 3 if any is left. The broker names no process itself: it holds the warden's
  pidfd, opened before confinement, and its seccomp filter allows `pidfd_send_signal` on that
  descriptor number only and `waitid`, and no `pidfd_open` or `kill` (with Landlock ABI 6 or
  later, `LANDLOCK_SCOPE_SIGNAL` too; the tested 6.8 kernel has ABI 4). A broker whose warden
  could not stop everything exits 4 (3 is a secret's object reached), and the launch then exits 2. A socket-entry supervisor is a
  subreaper too and does the same after each connection; if it cannot, or a connection's broker
  exits 4, it stops serving (exit 2) rather than run the next connection beside survivors. A run whose sweep passes its deadline fails
  loudly (a line on standard error; the runner exits and hands the rest to the warden): no loop
  is unbounded. Every process of the launch is non-dumpable (`PR_SET_DUMPABLE` 0 from the first
  instruction, reset only by a program's `execve`), so a program cannot ptrace or read the
  memory of the runner, broker, guest or supervisor. What stays, as the user accepted in
  choosing no namespaces: a program runs as the launching user, so it can signal the runner and
  the warden by number (`PR_SET_DUMPABLE` 0 stops ptrace, not `kill`). One that stops (SIGSTOP)
  or kills the warden and kills the runner can leave a descendant running: in a one-shot launch
  the warden's parent, the broker, is not a subreaper (it may signal nothing but the warden), so
  such a descendant ends under init; the broker then kills the warden, fails, and the launch
  exits 2, but the descendant lives. In a socket-entry server the supervisor (a subreaper) still
  stops it, unless the program stops or kills the supervisor too. Cleanup holds against
  programs that do not attack the launch's own processes; containing a hostile program needs a
  separate user or a namespace, which the user chose not to rely on (review of this round).
  The Exec budget a socket-entry server's brokers spend is in a page of its own that each
  worker unmaps before its sandbox, so guest code cannot write it back.
  Each program also gets `RLIMIT_NPROC` set to the user's process count when it starts plus the
  plan's `[limits] exec-processes` (default 256, shown by `native-plan`); the kernel does not
  apply it to root. A program holds no capability: when the launch has any (run as root), the
  program's securebits are set and locked (`NOROOT`, `NO_SETUID_FIXUP`, `KEEP_CAPS`,
  `NO_CAP_AMBIENT_RAISE`), its bounding set is dropped, its ambient set cleared and its
  capabilities emptied before `execve`: a program run as root is unprivileged root. No seccomp
  filter is put on a program.
  Tested under Docker's default seccomp profile, as root (and the process limit as another
  user): 1500 orphaned `setsid` descendants plus one that keeps forking; a guest deadline
  expiring under orphaned `setsid` descendants, with a bystander in the launcher's own group
  left alone; a program that kills the runner after leaving orphans (the warden stops them; in a socket-entry server, one that kills the runner and the warden too, the supervisor);
  `exec-processes = 20`; no capability and `no_new_privs` in a program.
- **The Exec builder natively**: `exec.command(name)` (library/wasm-exec.wy, docs/wasm-capabilities.md)
  needs four more operations, the same on both systems: input bytes (sent to the broker as
  `WITCHY_EXEC_INPUT` frames, pending with the arguments, written to the program's standard input
  through a pipe the runner polls beside its output, so a program that answers before reading all
  of it never blocks the run), a time limit and an output limit (checked in the guest runtime:
  more than a program's two seconds or than the plan's `exec-bytes` fails that run with code 7,
  `Wider`; the broker's own clamp stays), and standard error (a second pipe; the reply gives
  output and error lengths, read output first; both count against the output limit).
- **Exec variables and working directory**: an Exec entry may declare `env = ["LANG", "TZ"]`
  (the executable's own document; plain upper-case names, at most 16, each once): the plan
  carries them as an `exec-env` record, the review says "only the variables LANG, TZ (values
  from the program)", and `Command.env` gives only those names; the guest checks the name and
  the broker checks every entry again before the run (declared, once, value without NUL and
  at most 4096 bytes). `Command.cwd` moves a granted `Dir` to the run: the guest names the
  broker's slot (or root) for it, the broker duplicates its opened descriptor, and the run
  starts the program with `fchdir` on it; on Linux the descriptor goes to the runner with the
  request (SCM_RIGHTS on the runner's channel, the one descriptor the broker's seccomp filter
  lets `sendmsg` use). A directory can only be one this launch already opened under its grant
  checks (the program directory never overlaps a writable grant, and Exec is refused beside a
  secret), so no new check is needed and none is keyed to a path.
- **Signing on Linux**: the AWS-LC FIPS module (aws-lc-fips-sys 0.14.4) builds offline for
  Linux aarch64 from the crate's sources in the local cargo cache, in a local image that has
  cmake, a C compiler and perl (`3b-audit-nydus-linux:local`), with the Go toolchain copied out
  of the local `golang:1.27.3-alpine` image (statically linked; `GOPROXY=off`), single-threaded
  (the delocator runs out of the 1.9 GiB VM's memory otherwise). It is the static archive
  `libaws_lc_fips_0_14_4_crypto.a` (symbols prefixed `aws_lc_fips_0_14_4_`), linked whole into
  the executable; a compiler built with `WITCHY_NATIVE_AWS_LC_ROOT` pointing at its prefix
  admits `secret`, `store` and `object` records on Linux, with the same `sign/witchy-2/ROLE`
  profile and descriptor-only seed rules. Checked in the container: FIPS mode is on, the ACVP
  ML-DSA-65 key-generation vector gives the expected public key, a signature made after the
  guest sandbox is installed verifies, another role's domain is refused, and the language's
  `Signer` binds only a declared descriptor argument (not an unopened one, not a development
  flag, not a short seed); Exec beside a secret is refused at build.
- **no_new_privs and Landlock, by process** (Linux): `no_new_privs` is set at the first
  instruction of the launch and inherited by every process: the supervisor (socket-entry), the
  guest, the broker, the runner and every program (so a setuid or file-capability program gains
  no privilege through Exec). Landlock: the guest has an empty ruleset (no file system access at
  all, every handled right denied) plus a seccomp filter of its few relay calls; the broker has
  read rights on a read root, read and write rights on a writable root, write-make-remove-file
  on a written file's directory, and TCP connect to the planned ports only, plus a seccomp
  allow-list (with Exec, also `pidfd_send_signal` on the warden's pidfd only and `waitid`; no
  `pidfd_open`, no `kill`). The runner, the warden and the supervisor are not Landlock-confined: a Landlock domain is
  inherited, and the runner's programs run with the user's authority by design (they need the
  user's system files), as on macOS; the supervisor only forks, waits and signals its own
  children.
  The runner is unconfined by design: what it runs has the user's authority, as on macOS.
- **Socket-entry servers**: the supervisor, workers and per-connection brokers as on macOS;
  each connection's broker confines itself and forks its own runner.
- **Tasks** use the same cooperative scheduler and ARM64 stack switch as macOS.
  Each Linux task has inaccessible guard regions at both ends of its stack.
- **Refused without the AWS-LC FIPS build**: secrets (`[secrets]`, a store,
  and object identities of secrets).


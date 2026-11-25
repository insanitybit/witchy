# Capabilities of a program's own

`main` receives the host's capabilities (`Console<Write>`, `Console<Read>`,
`Allocator`); no source can make one. (`allocator.share()`, `clock.share()` and
`rand.share()` give a second handle to the run's one heap, clock or randomness, for a task's
input: each adds nothing that lending a reference does not.) A program
narrows one by putting it in a sealed record, as the book's `capability X from U` does
(capabilities-narrowing.md):

```
pub sealed type Logger:
    out: Console<Write>
    lines: U64

pub fn logger(console: Console<Write>) -> Logger:
    return Logger(move console, 0)

impl Logger:
    pub fn log(logger: &mut Logger, value: U64):
        logger.lines += 1
        logger.out.print_int(logger.lines * 1000 + value)
```

A function given a `&mut Logger` can log and nothing else.

- A record may hold a console or allocator. It is then not Copy: the authority
  is moved, never duplicated (`copied.refused.wy`).
- Only the sealed type's file builds one (`forged.refused.wy`).
- Other files may read a sealed type's plain fields, but not a field that holds
  a capability, at any depth (`reached.refused.wy`). Without this, `log.out`
  would hand the whole console to anyone with a logger.
- A capability given to a record is gone from where it was (`used-after.refused.wy`).
- A record holding a capability is not storable in a Vec or other Buffer.
- A capability, or a record or sum that holds one, is not a type argument to a
  generic function, and no trait is implemented for a capability. Generic code
  therefore never holds authority its signature does not show
  (`generic*.refused.wy`, `trait-*.refused.wy`). Two exceptions, each visible where it
  is made: a closure that took over a capability (docs/wasm-closures.md) and a `Task`
  (docs/wasm-tasks.md).

Fixtures: `tests/fixtures/capabilities/`.

## User-defined capabilities

`capability ConfigDir from Dir<Read>` (book: capabilities-narrowing.md) declares
a sealed record holding the capability: only its file can build one or take the
capability back out, so a function taking a `ConfigDir` demands the brand, and a
library decides what a holder may do with it (`capabilities/branded.wy`, where
`ConfigDir.load` reads `config.txt` and nothing else). Another file building one
or reaching inside is refused (`branded-forge.refused.wy`,
`branded-unwrap.refused.wy`), and so is a "capability" made from something that
holds no authority (`branded-empty.refused.wy`). The brand never hides authority:
`wasm-caps` reports `show: … Dir<Read> (in ConfigDir)`.

The block form carries policy beside the authority: `capability Table:` with
fields `net: Net` and `name: U64` is a sealed type whose file decides what a
holder may do (`capabilities/policy.wy`: a `Table` answers only for its own
table, and its `Net` is confined to one server by `only`). `wasm-caps` sums the
capability fields and ignores the rest: `report: Console<Write>, Net (in Table)`.
A block with no capability field is refused (`policy-empty.refused.wy`).

## A scoped SQLite transaction

A single-run program may take `db: Sqlite` in `main` and declare the
[SQLite package](../packages/sqlite/README.md). Import it as `import "@sqlite"`.
The operator selects an
existing database in a private directory and grants exact table names.
The SQLite worker cannot take network, filesystem, ordinary input or executable
grants. It may take one descriptor-backed `Secret<Seal>` to authenticate a
transaction. The database and its journal files cannot alias the secret source.

A one-shot data worker can receive a bounded request from its launcher with
`--sqlite-request-stdin --input-bytes=N`. Standard input must be a pipe
distinct from the secret source. The host reads it through EOF before the
guest starts, with the run deadline and a maximum limit of 16 MiB. Oversized
and incomplete requests stop the run. The snapshot is delivered to the
guest's `Console<Read>`. Table provisioning uses a separate launch.

```toml
[sqlite]
db = { file = "/var/lib/app/private/state.db", read = ["items"], insert = ["items"] }
```

`transact_checked(plan, bindings, blob, reply)` gives a typed result and runs
once. The lower-level `transact` method returns the raw byte count. A plan
has at most 16
newline-separated steps with tab-separated operation, failure label, and
SQL. Operations are `exec`, `change` for one changed row, `expect` for
exactly one integer 1, and `result` for exactly one nonempty BLOB. The
final step returns a result. Bindings use newline-separated
`name<TAB>text|int<TAB>value`; SQL names use `:name`. The engine
provides `:blob` and samples `:now` under the transaction lock.
Each statement is prepared with the selected table rights. The host
checks the reply fits before committing. The reply starts with 0 for
a committed result, 1 for a failed assertion, or 2 for an unavailable
transaction. An unfinished transaction rolls back.
Replies default to 16 KiB. The operator can set `reply_bytes` in the
SQLite grant or pass `--sqlite-reply-bytes=N` to the host, up to 2 MiB.
The reply must fit both that bound and the caller's reply buffer. Both
limits include the status byte.
Grants limited to `read` use a deferred read transaction on a read-only
connection. A workflow can take a write reservation on a separate connection
while the worker selects evidence. SQLite permits the concurrent reader
and keeps other writers out until the reservation ends.
[SQLite authorizer](https://www.sqlite.org/c3ref/set_authorizer.html)
· [SQLite transactions](https://www.sqlite.org/lang_transaction.html)

An operator can provision tables on the same file with a separate
`create` list of table names, or `--sqlite-create=items` on the host.
For that grant, supply a plan of one to 16 `exec` statements using
`CREATE TABLE`, with empty bindings and blob. The grant covers implicit
indexes for primary and unique keys. The host refuses data queries,
data writes, changes to existing definitions, and combinations with
table data rights or secrets. A successful reply is `0` followed by
`ok`. The operator creates the empty, private file before provisioning.

## A local service for a connection

`LocalService` lets a Wasmtime connection worker call an operator-selected
private service once. The launcher supplies a connected Unix stream with
`--local-service-fd=N`. The guest can exchange byte slices within the
connection deadline and network budget. Requests are capped at 1 MiB plus
16 KiB. Replies default to 16 KiB and can be explicitly bounded with
`--local-service-reply-bytes=N` up to 20 MiB. A failed frame retires the
stream and publishes no partial reply.

The grant belongs to a `main` that accepts a `Socket`. A grant document can
bind a parameter using `[local_service]` with `transactions = "fd:7"`.

## What a program can do: `wasm-caps`

```
$ witchyc wasm-caps tests/fixtures/fs/load-config.wy
main: Allocator, Console<Write>, Dir<Read>
...
load_config: Allocator, Dir<Read>
22 reachable functions hold no capability
```

Each function `main` can reach, with the capabilities its parameters hold:
directly, through a reference, or inside a record or sum. One held through a
record is named with it, as the book reports a refined capability: a function
given `&Logger` shows `Console<Write> (in Logger)`. Authority enters only at `main` and moves only through values,
and a capability cannot be a type argument, so this is exact: a function not
listed can print nothing, read no file and allocate nothing. Functions not
reachable from `main` are left out (src/compiler/caps.rs).

How the same program can fail, function by function, is `wasm-traps` (docs/wasm-traps.md).

### `wasm-caps-diff`: a gate on new authority

`witchyc wasm-caps-diff OLD.wy NEW.wy` (book: "The supply-chain payoff")
compares two versions of a program. It prints what each function gained and lost,
and exits 1 when the new version as a whole holds a capability the old one did
not (`widened: the program now holds Net`). Giving rights up is not widening: a
bare `Dir` (or `Secret`) covers its one-right views, so `Dir` to `Dir<Read>` is
reported as narrowed and exits 0, and `Dir<Read>` to `Dir` exits 1. `wasm-caps`
ends with a `total` line, the union over every reachable function. A CI step
running `wasm-caps-diff` against the last release fails when a dependency
update starts using the network, the disk or a subprocess. `wasm-caps --json FILE`
gives the same footprint for tools: `{"functions": {...}, "pure": n, "total": [...]}`.

### Per file: what each package may reach

`witchyc wasm-caps --by-file PROGRAM` reports, for each file of a program (the standard
libraries as `<standard>/...`), the functions `main` reaches there, what they can use, how many
have a contract Lean proved, and which perform unchecked memory operations (each shown safe by
its function's proof). An imported package can only use what the program passes it.
Every file of the program is listed, one that holds only constants or types too (`0 reachable
functions; can use nothing`): it is part of the program, so a policy must hold it to a line and
a draft pins it.

`witchyc wasm-caps --policy POLICY PROGRAM` holds each file to a policy and exits 1 with the
files that reach too far, so adding or updating a dependency cannot silently widen what it can
do. A policy kept beside the program, `app.policy` for `app.wy`, is applied by every build
(`wasm-checked`, `build`), before anything is proved: a program that does not fit it is not
built (`tests/fixtures/packages/overreach.refused.wy`). Paths in a policy are relative to the
directory the policy is in, wherever the check runs from. A package imported by name is named
`@name` (and its other files `@name/PATH`) wherever its copy is (docs/wasm-packages.md). Each line is `path: capabilities`: a
path ending in `/` covers every file under it, any other path exactly one file, and the
closest line applies:

```
<standard>/: *
app/: *
deps/config/: Allocator ; no unchecked
```

`*` allows anything, `nothing` allows nothing, and `; no unchecked` forbids proof-only memory
operations such as `memory.utf8_unchecked` in reachable functions. The runtime-validated
`str.from_utf8_bytes` is allowed. A file no line covers is itself a violation, whether or not `main`
reaches a function in it: a new dependency is reviewed before it is allowed anything
(`tests/fixtures/packages/config-app.policy`; unit test `a_policy_names_each_file_that_reaches_too_far`).
`witchyc wasm-caps --draft-policy PROGRAM` prints a policy the program fits as it is, one
line per file with exactly what it uses (`; no unchecked` where it performs none) and its
digest: a start to review, narrow to prefixes, and keep beside the program.

`; digest sha384:HEX` pins a line to the code it was reviewed for: the SHA-384 of the one file
it names (its own bytes, as the build read them: `shasum -a 384` of the file, whatever the
program imports around it), or, for a directory, of `name NUL pin LF` for each file of the
program the line governs (name relative to the directory, in name order; reached or not; each
pin `sha384:HEX`). The algorithm is part of the pin (the `witchy-2` profile, decisions.md
2026-11-20). A bare 64-digit digest, the SHA-256 pins written before, is refused with what to
do, never compared; `wasm-caps --repin`, run after reviewing the code, replaces it only when it is
still the code's SHA-256 (as pins were then), and refuses one the code no longer matches. A package changed
since, a file added under it included, is a violation that prints the digest it has now, so an
update is reviewed before it runs; after reviewing it, `wasm-caps --repin POLICY PROGRAM` sets
the digests to the code as it now is (unit tests `a_digest_pins_the_code_a_line_was_reviewed_for`,
`repinning_updates_only_digests`; docs/wasm-packages.md;
the policies in `tests/fixtures/packages/`, which the fixture runner checks).

## `pure fn`

`pure fn square(x: U64) -> U64` is checked to be given no capability: no
parameter holds one, directly, through a reference, or in a record or sum. Since
capabilities arrive only as values (a closure holding one counts as holding it), such a
function can compute and trap, and nothing else; it cannot allocate (that needs an
`Allocator`). Methods may be `pub pure fn`. Fixtures: `tests/fixtures/pure/`.

Directories and files: `Dir<Read>`, `Dir<Write>`, `File<Read>`, `File<Write>`
(docs/wasm-files.md).

`Clock` and `Rand`: `clock.now_ms()` is milliseconds since the Unix epoch by the
host's clock (a duration literal, `250ms`, `30s`, `1m30s`, `2h`, `1d`, `1w`, is a `Duration`
from `import time`, whose `.ms()` is that count: `arithmetic/durations.wy`; `clock.since(start)`
is the Duration since a reading and `clock.sleep_for(40ms)` sleeps for one:
`host/clock-duration.wy`, `net/timeout-duration.wy`); `rand.next_u64()` is 64 bits from the operating system's secure
source, and `import rand` adds an unbiased `rand.below(n)`. Each is its own
grant (`--clock`, `--rand`); a program that asks for one the run does not grant
does not start (`tests/fixtures/host/`). A function that reads the time or draws
a number has the capability in its parameters, so `wasm-caps` lists it and a
`pure fn` cannot. Waiting for a time is reading it too: a timer for `listener.poll`
(`clock.timer(due)`, which `clock.sleep(ms)` uses) is made with the clock, and a poll
entry naming no timer, socket or listener of the program reads as closed.

A capability may sit in a sum: `Option<Dir<Write>>` is "maybe somewhere to write"
(the book's optional capability, `fs/optional.wy`), and `wasm-caps` counts it.

The host side is one mechanism (src/compiler/host.rs): each family is an authorizer
that hands `main` its value and a list of operations of type `(i64, i64) -> (i64)`,
imported together and only when an emitted function holds one of the family's
values. A new host capability is a family and its operations, not a case in
every compiler pass.

`Env`: `env.get(&allocator, "NAME")` (`import env`) is a variable's value as a
`String`. The caller grants each variable, `--env=NAME=VALUE`, or `--env=NAME`
to pass on its own value; the launcher hands the values to the engine in its
otherwise empty environment, never on its command line, and the engine reads
only those. A name the
run did not grant is indistinguishable from one that is not set, so a program
cannot probe the caller's environment (`host/env.wy`, where `HOME` is set in the
caller's environment but not granted).

`Net`: TCP connections to the endpoints the run grants, `--net=HOST:PORT,...`,
each matched exactly as written (`import net`). `net.only("host:port")` is a `Net`
that reaches that one endpoint, if this one reaches it, so a helper given it can
dial nowhere else; it never widens. `net.connect("host:port")` gives a `Socket`
with `read_some(&mut buffer)` (the count received, 0 at the end), `write_some(&bytes)`
(the count the connection took) and `write_all`, which move a checked slice in one host
call and, on failure, say how many bytes went through first (`NetFailure`); and the
conveniences `send`, `send_string`, `next_byte`, `read_text` (to the end) and `close`. The
host sets `TCP_NODELAY`: it writes when the program sends in bulk or is about to wait for
an answer, never a byte per write. `scripts/bench-echo.py` compares a Witchy echo server
with native Go on loopback.

`socket.read_exact(&mut buffer)` reads exactly the requested bytes without
reading ahead. An empty slice succeeds without I/O. `UnexpectedEof(done)`
reports how many bytes arrived before EOF. `ReadFailed(NetFailure(error,
done))` preserves the underlying transport error and the number of bytes
already copied into the destination. `read_exact_async` can suspend while
waiting on the socket and reports `Cancelled` with the same prefix count.

Many connections at once go in a `Group`, a capability holding the sockets moved into it:
`listener.group()` (or `net.group()`), `group.add(move socket)` gives a slot, a plain number
a `Vec` can hold, and `group.take(slot)` gives the socket back, so code handling one
connection holds only that socket while it does. `group.wait(&mut entries)` blocks (`poll`)
until an entry (`wait_entry(slot, read, write)`, or `LISTENER_SLOT` for the listener the
group was made from) is ready and marks each. `socket.try_read`, `try_write` and
`listener.try_accept` never wait: `Err(WouldBlock)` when there is nothing to do. A socket
cannot be a `Vec` element, since a capability is never a type argument; the group is how a
program holds many. Closing a group closes what is still in it. The host reuses the numbers
of closed handles, so a server runs as long as it likes. `examples/echo/server.wy` is a
single-threaded concurrent echo server built on this (`net/group.wy` covers the API).
`--net-bytes=N` bounds what a run sends and receives (1 MiB by default).
`tests/fixtures/net/reach.wy` checks grants, refusals and narrowing; a round trip
with a real server was checked by hand (a local echo server answered
`you said: hello over tcp`), since the fixture runner starts no servers. The host
matches the address as a string before resolving it: a granted hostname is
trusted to resolve where the grant meant (no private-range filter yet).

`Args`: the words the operator gave this launch (`import args`): `args.count()` and
`args.get(&allocator, index)`, `Err(Missing)` past the last. Granted by the document's
`[args] args = true` (16 words, 4096 bytes) or `{ count = N, max-bytes = B }`, which is the
engine's `--args=COUNT:BYTES`; the words are the launcher's `--arg=WORD`, one each, refused
without the grant or over it (`host/args.wy`, `host/args-over.traps.wy`). A word is bytes:
never a flag, never a path the engine opens.

**A program for one connection: `main(connection: Socket)`.** A `main` that takes a `Socket`
instead of a `Listener` is run once for each connection: the engine listens on the granted
address (`[listen] connection = "127.0.0.1:8080"`, the same grant; `wasm-grants` adds
`--instance-per-connection`), accepts, and starts a new instance of the module for each
connection, with that connection as the `Socket` and everything else `main` takes granted
again. Memory, the heap and every handle are new each time: nothing is in memory between
connections, and state that outlives one is kept where the host holds it, in a granted `Dir`.
An instance that traps, runs out of its instruction count (`--fuel`, each instance's own) or
outlasts `--connection-deadline-ms` (default 10000) ends alone: its connection is closed,
one line says so (with the function it stopped in and the check, as a run that traps alone
is told), and the next connection starts from the module as built. The byte budgets are the
run's and are counted on across instances; beside each, an operator may give each connection
its own (`--net-bytes-per-connection`, `--fs-bytes-per-connection`,
`--fs-entries-per-connection`, `--output-bytes-per-connection`), which ends only that
instance's use of it, and then may write the run's as `unlimited` (refused without its own
beside it, and for a program run once). An instance starts with its own share, or what is left
of the run's where that is less. One connection at a time; the engine accepts ahead of it and
holds up to `--waiting-connections` (default 1024) waiting, so a burst past the system's own
queue (128 on macOS) is not reset. Such a `main`
takes no `Listener`, `Net`, `Exec`, `File`, `Console<Read>`, `SecretStore`, `Secret` or `Signer`
(refused by the compiler and the engine): each is read once or held across connections,
which an instance made again for each connection cannot be given whole. A `main(listener:
Listener)` server is unchanged: one instance, its own accept loop, its state in memory, and
a limit that ends the run (docs/decisions.md, 2026-11-22, "An instance for each connection").
A `TlsSigner` is available to per-connection instances, with a fresh handle for each.
The host holds the key and exposes only the TLS 1.3 server CertificateVerify operation.
See [HTTPS on Wasmtime](wasm-tls.md#serve-https-on-wasmtime) for the launch profile.

A supervisor can pass `--listener-ready-stdout` for a per-connection
service. The host writes one `READY\n` line after checking and compiling the
module, opening the grants, and binding the listener. Reserve standard
output for this signal. The flag is refused with a console output grant.

`Listener`: the one address a run may accept connections on, `--listen=HOST:PORT`,
bound by the engine before the program starts (an address it cannot bind stops
the run there). An IPv6 address is `[::1]:8080` in a grant document and to the
engine, which also takes `::1:8080`, as `wasm-grants` writes it (a flag holds no
bracket). No second socket can bind the address while the run holds it. A socket
the listener accepted names the granted authority (`socket.accepted_on`), which is
what an HTTP server checks a request's `Host` against (docs/wasm-http.md). `listener.accept()` gives the next connection as an ordinary
`Socket`, or `Err(TimedOut)` when no one connects within two seconds. A
`Listener` dials nothing and a `Net` accepts nothing (`net/listener-dial.refused.wy`,
`net/net-accept.refused.wy`); they share the host's network table, and
`net_authorize` hands out each only under its own grant. `net/greet-server.wy`
runs with no client in the fixtures (it times out and says so); by hand, with
a Python client connecting to `127.0.0.1:47613` and sending `witchy\n`, the
server printed `6` and the client received `hello, client; you sent `.
`wasm-caps` on it:

```
main: Console<Write>, Listener
serve: Listener
```

`tests/fixtures/programs/fileserver.wy` puts these together: a client sends
`GET name`, and the server answers with that file from one directory. By hand,
`GET hello.txt` returned the file, and `GET ../secret.txt` and `GET missing.txt`
both returned `not found` (the host refuses the first path before looking).
`wasm-caps`:

```
main: Allocator, Console<Write>, Dir<Read>, Listener
answer: Allocator, Dir<Read>, Socket
next_is: Socket
request: Allocator, Socket
serve: Allocator, Dir<Read>, Listener
```

Nothing in it can write a file or dial out, and the parsing (`request`,
`next_is`) can touch only the connection it is given.

`Exec`: the sharpest authority, since what it starts runs outside the sandbox
(book: capabilities-narrowing.md). The run grants names in one directory,
`--exec=/abs/dir:greet,git`, and an `Exec` may run only those (`import exec`):

- A name is a plain file name in the list. The host opens it from the granted
  directory with `O_NOFOLLOW` (a link there is not followed), requires a
  regular file with an execute bit, and copies it into a fresh private 0700
  directory before starting it, so nothing can swap the program between the
  check and the start (macOS has no `fexecve`, so the copy is the same on both
  systems).
- No shell: `exec.arg(text)` adds one argument, whole; `"world; rm -rf /"`
  arrives as one argument and nothing interprets it. Arguments apply to the next
  `run` only. The program gets an empty environment, no input unless a `Command`
  gives it some, its standard error kept for the caller (`Run.read_error_text`), and
  its own process group.
- `exec.run("greet")` waits for the program and gives a `Run`: `code()`, and
  what it wrote, by `next_byte()` or `read_text(allocator)`. A program still
  running after two seconds is stopped with its whole group (`Err(TimedOut)`),
  as is anything it left in that group once it exits. The host registers the
  group before authorizing the executable to start and keeps its leader's
  PID reserved through cleanup. A host deadline, shutdown signal, or closed
  launcher lifeline stops the registered group. `--exec-bytes=N` bounds what all
  programs together may hand back (1 MiB by default).
- `exec.only("greet")` narrows to one name; it never widens.
- `exec.command("greet")` configures one run (`host/exec-command.wy`):

  ```
  match exec.command("answer").arg("a b").input_text("in").deadline_ms(500).output_limit(200).run():
      Ok(run):
          ... run.code(), run.read_text(allocator), run.read_error_text(allocator)
      Err(Wider):
          ... a setting asked for more than the grant allows
  ```

  Each setting only narrows: `deadline_ms(n)` is at most a program's own two seconds
  (more is `Err(Wider)`, 0 is `Err(InvalidName)`), and the run's own deadline still
  applies; `output_limit(n)` is at most what the run grants all programs (`--exec-bytes`,
  `[limits] exec-bytes`; more is `Err(Wider)`), counts standard output and error together,
  and what is left of that budget still applies; `input(bytes)` / `input_text(text)` is
  what the program reads on standard input, counted with the arguments against the 1 MiB
  that may wait to be run. `env(name, value)` gives a program one variable, only of a name
  the Exec grant declares (`env = ["LANG", "TZ"]` beside `dir` and `programs` in the grant
  document, `--exec-env=LANG,TZ` for the engine): a plain upper-case name, a value without
  NUL of at most 4096 bytes, each name once, at most 16; any other name fails the run with
  `NotGranted` (a malformed or repeated one with `InvalidName`), and nothing else of any
  environment reaches the program (`host/exec-command-env.wy`). `cwd(dir)` (a `Dir<Read>`,
  or `cwd_writable` for a `Dir<Write>`; a full `Dir` is either) gives a granted directory up
  to the run, which starts the program in it: the engine and the native runtime pass the
  directory as the grant opened it (a descriptor, `fchdir` before exec), never a path, and
  the handle is moved, so the program cannot use it after (`host/exec-command-cwd.wy`,
  `exec-command-cwd-used.refused.wy`). Without one the program runs in its private
  directory. A working directory is only where the program starts, not a limit on it: the
  program runs with the user's authority (`cwd` with a `Dir<Read>` does not stop it writing
  there or leaving it); the split by right only says which `Dir` the Witchy program gives up.
  Variables that make a program load code or configuration from a path the Witchy program
  chooses (`LD_*`, `DYLD_*`, `BASH_*`, `PATH`, `HOME`, ...) can never be declared: one list,
  src/compiler/exec_env_refused.txt, read by the compiler, the native broker and the engine. A secret never meets either: Exec is refused beside a secret. A name the `Exec` may not run is refused by
  `run` (`NotGranted`), also through an `only` narrowing. `Run` gains standard error:
  `next_error_byte()`, `read_error_text(allocator)`.

  What keeps a `Command` from widening: it is `pub sealed type Command` owning an `Exec`
  narrowed to its one program, exactly what `only` gives. Only `wasm-exec.wy` builds one or
  takes its `Exec` out (`exec-command-forged.refused.wy`, `exec-command-taken.refused.wy`);
  it is not copied, so it runs once (`exec-command-copied.refused.wy`); a value holding a
  capability is never stored in a Buffer or Vec (`exec-command-stored.refused.wy`). It does
  not borrow the `Exec` it came from, and that is the argued outcome of trying (decisions.md,
  "Exec builder lifetime"): no record may hold a `&mut` (W0403), and a view holding `&Exec`
  cannot use or hand on an owner it also holds, so a borrowing builder would have to keep its
  arguments in the source `Exec`'s shared pending state, where two commands from one `Exec`
  would mix. What it holds instead cannot widen anything: an `Exec` the library narrowed to
  one program through the host's own `only` check, the same authority `only` already hands a
  program as an owned value; capabilities do not expire, so outliving the borrow carries
  nothing the program did not already have; and only `wasm-exec.wy` can build or open one. The same holds natively (`native_exec_command_builder_narrows_only`)
  and on Linux (`tests/native-linux`, raw operations).
- The program's directory may not overlap a writable grant (`--write-dir`, or
  `--dir` with write rights): a `rename` there could otherwise put an existing
  executable under a granted name. The engine refuses such a run before it
  starts.
- A run ended from outside stops its program too: the launcher sends SIGTERM,
  and the engine kills the running program's group before it exits (SIGKILL
  follows 200 ms later). A program never runs past the run's deadline.

What stays the caller's responsibility, from an independent review of this
code: the module chooses every argument, so granting a general tool (`sh`,
`env`, `find`, `xargs`, `git`) grants arbitrary commands; grant single-purpose
programs. A program that daemonizes (`setsid`) leaves its group and is not
stopped; the engine stops waiting for its output after 200 ms. A copied program
loses files beside it (libraries found relative to it), and fails if the
temporary directory is mounted `noexec`. A deadline that falls between a
program's start and the engine recording its group can miss it (a window of
microseconds). Pending arguments across all `Exec` values are bounded to 1 MiB.

What a started program gets of the run's secrets. It inherits no descriptor from 3 up: the
engine marks every descriptor it was started with close-on-exec before anything else runs,
and closes a secret's descriptor once it has read it, on success and on error. The engine
takes each secret's variable out of its own environment before anything else runs: it
overwrites the bytes where the process was started with them, then unsets the variable, so
neither the empty environment the program gets nor what another process of the same user can
read of the engine's (`/proc/PID/environ` on Linux, `ps -E` on macOS) holds the value. On Linux
the engine also makes itself non-dumpable (`PR_SET_DUMPABLE` 0) first, so a process of the same
user cannot read its memory or `/proc/PID/environ` at all, whatever the Yama setting (reasoned,
not run: the Linux path is untested here). What stays: on macOS the engine's memory holds every
value it read, readable by a same-user process with debugger rights; the launcher that started the engine with a variable still has its own copy, and its
own initial environment, unless it took it out too; overwriting keeps the value's length (the
bytes become NUL). A launcher should deliver a secret on a private descriptor (`fd:N`) rather
than in the engine's environment (Grimoire does: "Grimoire launches"). Isolation from all of
this needs a program running as another user or in a sandbox of its own, or no `Exec` grant.
`host/secret-slot-exec-child.wy` starts a program that looks for canaries in every descriptor
it inherited and in the engine's environment as `ps -E` or `/proc` shows it, and finds none
(the engine before this change showed both).

Also from that review: receiving on a socket now counts every byte against
`--net-bytes` (it could overrun by up to 4 KiB). `rename` and `remove` look at
a path by opening it for reading, so an entry without read permission cannot be
renamed or removed.

`host/exec.wy` checks all of this against `host/exec-bin/` (a script, a link
to `/bin/ls`, a file without an execute bit); a two-second program was stopped
and left no process behind. The book passes the binary's directory as a
`Dir[Read]` beside the `Exec`; here every host operation has one capability as
its receiver, so the directory is part of the `Exec` grant instead.

`SecretStore` and `Secret` (book: cookbook-crypto.md): named host secrets with
two rights (`import secret`). The run grants each by name: `--secret=NAME`
(read and sign) or `--secret-seal=NAME` (sign only). The value comes from the
caller's own variable `NAME` (or `--secret=NAME=VALUE`, for tests) and reaches
the engine only in its environment, never on its command line; or from a
descriptor the engine was started with: `--secret=NAME@fd:N` and
`--secret-seal=NAME@fd:N` (N from 3 to 1023), which is how a launcher hands one
over on a pipe of its own (Grimoire does: "Grimoire launches"). The program
still asks for `NAME`; only where the engine reads it changes.

A store's descriptor is a source exactly as a fixed secret's `fd:` is (below): the
engine takes it when it reads the flag (only one it was started with; no number
twice, whether a slot's or a store's), reads it to its end with the same bounds
(64 KiB, the end within the one-second read bound, after the input check and before
the input is read), and closes it, on success and on error; failures name the
secret and the kind, never a value (`secret NAME: descriptor not closed in time
(fd:N)`, `unreadable descriptor`, `input too large`). Its device and inode join the
secrets' objects, so one source, one route holds between store secrets and slots:
two descriptors on one file or pipe (`secret-store-fd-reopen`), one number for a
slot and a store secret (`secret-store-fd-twice`), an `--input` that is the
descriptor (`secret-store-fd-input-alias`), a `--file` that is its file by any name
(`secret-store-fd-file-alias`), a slot's `--secret-object` naming it
(`secret-store-fd-object`), and every read, write, removal or rename through a grant
are refused as for a slot (the messages say "a secret slot's source" for both). A
store secret's name granted through `--env` is still refused
(`secret-store-fd-env-overlap`). There is no `--secret-object` for a store's
secret: Grimoire reads a store's secrets only from variables, which have no file; a
launcher that gives the engine a store secret's file directly as `fd:N` gets the
check on that file from the descriptor itself. `secret-store-fd.wy` signs with a
sealed key from a pipe and reads and signs with a token from a file, each giving
Python's HMAC; `secret-store-fd-held` and `-unreadable` are the read failures.

- `secrets.get("NAME")` gives a bare `Secret`, which has both rights and is
  passed where `Secret<Reveal>` or `Secret<Seal>` is expected, by value or by
  `&`, as a bare `Dir` is.
- `Secret<Seal>` signs by handle: `key.sign(allocator, message)` is
  HMAC-SHA-256 with the secret as key, computed in the engine, as 64 hex
  digits. It has no operation that reads the key, so a function that only signs
  says so in its signature and the checker holds it to that
  (`host/secret-seal-reveal.refused.wy`). Rights are never regained
  (`host/secret-widen.refused.wy`).
- `Secret<Reveal>` reads the value: `secret.reveal(allocator)`. A secret granted
  with `--secret-seal` is never a full `Secret`: `get` answers `Err(SealedOnly)`,
  and `secrets.get_sealed("NAME")` gives a `Secret<Seal>` under either grant, so
  the type always says what the run allows. The engine checks the same thing
  whatever a module asks.

`host/secret.wy` signs `release 1.0` with key `k3y` and prints the same digest
as Python's `hmac` (`8acdbced…b3bc`); `wasm-caps` shows
`endorse: …, Secret<Seal>` and `show: …, Secret<Reveal>`. The engine's
SHA-256 and HMAC (tools/wasmtime-host/src/sha.rs) are AWS-LC's (since 2026-11-20; see
"Crypto"), checked against Python on six vectors, including the padding boundaries at 55, 56
and 64 bytes, and RFC 4231's long-key case: the tags are those the handwritten HMAC gave.
Public-key signing is a `Signer` ("Crypto").

### Fixed secrets

A program that needs one particular key takes it as a parameter of `main`, with the one right
it needs. No store, no lookup by name at run time:

```
fn main(signing: Secret<Seal>, token: Secret<Reveal>, console: Console<Write>, allocator: Allocator):
```

Each such parameter is a slot of its own, named by the parameter: two `Secret<Seal>`
parameters are two keys, bound independently. A bare `Secret` (both rights) is not a `main`
parameter. The grant document approves each by name in `[secrets]` (see "Grant documents"):

```toml
[secrets]
signing = { use = "seal", from = "env:SIGNING_KEY" }
token = { use = "reveal-bytes", from = "env:REPO_TOKEN", also_from = ["fd:3"] }
```

- `use` must be the type's: `"seal"` for `Secret<Seal>` (sign only), `"reveal-bytes"` for
  `Secret<Reveal>` (read only; it cannot sign). The store's `"reveal"`, which reads *and*
  signs, is not a fixed secret's use.
- `from` is the source, required: `env:NAME`, a variable of the engine's process, or `fd:N`
  (3 to 1023), a descriptor it was started with, read to its end. `also_from` lists further
  sources the author allows; the engine never selects among them, the launcher does
  (`grimoire run --bind NAME=SOURCE`). No other field is accepted.
- A source belongs to one slot: it is refused when another slot lists it, when an `env:NAME`
  is also granted through `[env]`, or when it is a `SecretStore` secret's variable.
- A fixed secret's entry that no parameter takes is an error, not an over-grant warning.

`wasm-grants` turns each into one engine flag that names the source, never a value:

```
--secret-slot=NAME:USE:SOURCE      e.g. --secret-slot=signing:seal:env:SIGNING_KEY
```

The compiler lists `main`'s secret parameters, by name and right in `main`'s order, in the
module's `witchy.secret-slots` custom section (after the code, before `name`; the profile
admits only those two). Before the program starts, the engine binds each flag to the slot of
its name and reads every value. It stops the run, saying `secret slot NAME:` and a kind of
failure and never a value, when a slot has no binding, a binding has no slot, a use is not the
slot's type, a slot or source is selected twice, a variable is unset (`missing input`), a
descriptor is not one the engine was started with or cannot be read (`unreadable
descriptor`), a descriptor's writer has not closed it in time (`descriptor not closed in
time`), or a value is over 64 KiB (`input too large`). A descriptor is read once, to its end,
and closed, on success and on error (one still waiting at the time bound is held until the
run stops, which it then does); no program the run starts inherits it (nor any other
descriptor the engine was started with: see `Exec`). The end is a read that returns no bytes,
and only that: the engine reads with ordinary blocking reads on a thread of its own and waits
for it until the bound. On macOS `poll` never reports the end of a named FIFO whose writer has
closed, so the engine once waited out the whole bound with the value read
(`a_named_fifo_ends_when_its_writer_closes`, a host unit test, has a writer that closes and one
that holds the FIFO open). The end must come within one second of
the engine starting to read its descriptors (all of them together), and 100 ms before the
run's deadline at the latest: a writer that never closes is named by its slot rather than
reported as the run's timeout. The bound is in time because a value's framing is "everything
until the end": a size bound alone cannot tell a slow writer from one that never closes, and the
engine already bounds its whole run in time. A variable is taken out of the engine's
environment before anything else runs, whether or not the run then starts. A fixed secret's handle has exactly its use's
right, checked by the engine on each operation: a `Seal` slot reads as "sealed only" (257),
and a `Reveal` slot's signing operations stop the run. `main` receives each slot once, and a
fixed secret's grant is never a `SecretStore`.

`secret.reveal_bytes(allocator)` gives the bytes as they are (a `Vec<U8>`); `reveal` gives
them as text, and `Err(NotText)` when they are not UTF-8, instead of reading each byte as the
character of that number (which changed bytes of 128 and more).

`host/secret-slots.wy` signs with two `Secret<Seal>` keys declared in opposite orders in
`main` and in the flags, each giving its published HMAC-SHA-256 vector, and reads `00 80 ff`
back exactly from a descriptor. `host/secret-slot-*.traps.wy` stop before `main` for each
failure above; `secret-slot-canary` checks that neither output nor error repeats a value
the engine had read. `secret-slot-seal-reveal.refused.wy` and `secret-slot-full.refused.wy`
are the checker's side. An `--input` that is a secret's own descriptor or file (through `/dev/fd`, `/proc`, a link
or another name) is refused (`input is a secret slot's source`,
`secret-slot-input-alias.traps.wy`), and the input is read only after the secrets.
One source, one route, between slots and file grants too (review of 33face12). Two `fd:`
slots whose descriptors are the same file or pipe (a file opened twice, a duplicated
descriptor) are refused, by device and inode (`secret-slot-reopen.traps.wy`: the sealed
key's file opened again for a `reveal-bytes` slot). A `--file` or `--write-file` whose target
is an `fd:` source's file, under its own name or a hard link, is refused before `main` runs
(`secret-slot-file-alias`, `secret-slot-file-link`, `secret-slot-write-file-alias`).
Every read of a file's contents through a grant (`--file`, `--dir`, `--write-dir`'s append)
checks the descriptor it opened against the same list and stops the run if it is one
(`the program reads a secret slot's source as a file`, `secret-slot-dir-read.traps.wy`, a
hard link inside a granted directory). What this does not cover: an `env:` source has no
file to compare; a source the launcher fed through a pipe or a copy of the file is a
different object, so a `--dir` holding the original file or a copy is not refused (only a
read of the very file the engine was given is); listing a directory, or asking what kind of
thing is at a name, still shows that the file exists; a program started through `Exec` reads
by path as the same user and is not checked at all. Keep a secret's file outside every granted
directory: a `--dir` that contains it is the launcher's mistake, which the engine catches only
for the descriptor's own file (and the files a launcher names with `--secret-object`, below).

Writes too (2026-11-20). A program with `--write-dir`, or a `--dir` with write rights, over the
directory holding a key file could replace that file (a write replaces by rename), remove it,
or rename a file of its own onto it, and the next run would sign with a key the program chose.
Every write, `remove` and `rename` that changes an existing entry now first looks at what is at
that name, by device and inode (cap-std's `symlink_metadata` on the directory's own `Dir`, without
following a link), and stops the run on a secret's source (`the program writes, removes or renames a
secret slot's source`). A write is checked when the file is opened and again at `finish`,
where the replacement happens, since a rename meanwhile could have moved the key to its name.
A `rename` checks what moves and what it replaces; a directory that moves is looked through,
without following links, and one that holds a source anywhere below is refused the same way.
One with more than 65,536 entries below is not moved (an I/O failure: it cannot be checked
whole). A symbolic link is never followed by a grant, but a launcher may read a key through
one: a link that resolves to a source stops the run the same way, and any other link is not
replaced, removed or moved while the run has a source to compare (`invalid path`), nor is a
directory holding one (review of 499f8240: renaming a file onto `cur -> key` swapped the key
the next run read; `secret-slot-rename-onto-symlink`, `secret-slot-rename-dir-symlink`). Fixtures: `secret-slot-write-replace`, `secret-slot-write-link` (a hard link),
`secret-slot-remove`, `secret-slot-rename-onto`, `secret-slot-rename-from`,
`secret-slot-rename-dir` (`keys/` renamed and recreated with another key), and
`secret-slot-write-beside.wy`, which writes, renames and removes its own files beside the key.
What stays: between the check and the rename a process outside the engine (or one started
through `Exec`) can still move things; nothing in the module can, as it runs one step at a time.

`--secret-object=SLOT:DEV:INO` names the file a launcher read slot SLOT's value from, when it
hands the value over on a pipe (Grimoire does): the device and inode, never a path or value.
The engine adds it to the objects above, so a read, write, removal or rename of that file
through any grant, under any name, is refused as if it were the slot's own descriptor. It is
accepted only for a slot whose source is `fd:` (an `env:` value has no file), once per slot, and
is refused when it is another slot's source (one source on two routes); naming the slot's own
descriptor's file again changes nothing. It can only add restrictions: the list is consulted
only to refuse, the slot's own descriptor stays on it, and a wrong identity at most refuses a
file the program needed. `secret-object-file` (a `--file` that is a hard link to the file
behind the pipe) and `secret-object-write` stop; `secret-object-no-widen` gives a wrong
identity for a slot read from the key file itself, and a `--file` of that file is still
refused; `secret-object-env`, `-twice`, `-no-slot` and `-other-slot` are the refusals.
A store's inline `--secret=NAME=VALUE` is refused with `--exec`: a started program could read
the engine's arguments. `secret-slot-fd-held.traps.wy` holds its descriptor's writer open
and stops on that slot within the deadline; `secret-slot-exec-child.wy` is under `Exec`.
A store's secret (`--secret=NAME`) is taken out of the environment the same way, and a
variable that is both a store's secret and granted through `Env` is refused
(`secret-store-env-overlap.traps.wy`): taken out, `Env` would otherwise read it as absent.
Grimoire reads each source itself and hands the engine a private descriptor ("Grimoire
launches").

#### Grimoire launches

`grimoire run` (the Grimoire repository) launches a program with fixed secrets. Before it
fetches or builds anything, it reads the executable's own grant document's `[secrets]`,
selects each slot's source (`from`, or `--bind NAME=SOURCE` naming one of `from` and
`also_from`), and reads every value itself, with the engine's bounds (64 KiB; a descriptor
must end within five seconds); any failure stops it with the slot and the kind of failure. It
takes every variable outside the toolchain's out of its own process when it starts, as the
engine does, so no fetch, check or build step is given one or can read it from Grimoire; an
`env:` source the toolchain is given (the named `WITCHY_*` variables the compiler and its
proof helper read, `PATH`, ...) is refused. Two `fd:` sources that are the same file or
pipe (device and inode) are refused, as is a `--file` grant that is an `fd:` source's file
(by device and inode, so a hard link too), checked when Grimoire starts the engine. That is a
pre-check: a name can become the key's file after it. It then gives the engine each value on a fresh pipe of its own, passing only those
descriptors, and rewrites the slot's flag to `fd:N`: the engine's environment is empty and its
arguments name descriptors, never a value. For a slot it read from a regular file it adds
`--secret-object=SLOT:DEV:INO`, so the engine itself refuses that file through every grant, at
each open and write (Grimoire's `secret_run.py` replaces the granted file with a hard link to
the key after Grimoire's check, during the build, and the engine refuses it). `wasm-grants` must propose exactly the slots
Grimoire read, with the same use and declared source, or the run stops before the build.

It launches a `SecretStore` the same way (`[secrets] store = { TOKEN = "seal" }`). Each store
secret's source is the launch variable of its name, read by Grimoire before anything is
fetched or built (`secret TOKEN: missing input (env:TOKEN)` otherwise), and given to the engine
on a fresh pipe: `wasm-grants`' `--secret-seal=TOKEN` becomes `--secret-seal=TOKEN@fd:N`, so the
engine reads no variable and its environment stays empty. Refused before anything runs, by
one source, one route: a name the toolchain is given or any `WITCHY_` name; a name that is also
a slot's `env:` source (in `from` or `also_from`); a name, or a slot's `env:` source, also
granted through the document's `[env]` (the compiler and engine refuse these too, but only after
the fetch). The `[files]` gate applies whenever the program takes any secret, a store's
included. `wasm-grants` must propose exactly the store secrets Grimoire read, with the same use;
a store table no `SecretStore` parameter takes stops the run before the build, since its values
were read for nothing. A store secret and a slot may share a name: each has its own pipe.

Not yet: TLS; an
allocator with a budget (that needs the allocator's trusted helpers to keep a
budget per handle, and they are left unchanged).

## Crypto

`import crypto` (library/wasm-crypto.wy). The engine computes with AWS-LC's FIPS module
(`aws-lc-rs` with `fips`; docs/decisions.md, 2026-11-20); no FIPS claim is made. The suite is
`witchy-2` (ML-DSA-65 and SHA-384, the approved profile of 20 November 2026); the first one,
`witchy-1` (ML-DSA-44), is retired and nothing verifies or signs it (decisions.md, "witchy-2").

**Public computation needs no capability.** `sha256(data, digest)` (a 32-byte `&mut [U8]`),
`sha384(data, digest)` (48 bytes, used by the registry profile),
`sha512(data, digest)` (64 bytes), `mldsa87_verify(public_key, message, signature)`
(pure ML-DSA-87 with empty context, a 2592-byte key and a 4627-byte signature),
`mldsa65_verify(public_key, message, signature)` (FIPS 204 ML-DSA-65, empty context, the whole
message) and `witchy2_verify(public_key, role, message, signature)` (the message must begin
with `witchy/v2/ROLE\0`, then ML-DSA-65) are `pure fn`s: they take only slices, so any
function may call them. The underlying public host imports take no authorizer or handle.
The engine checks every range against the program's memory before it reads one. A key or signature of another
length is invalid (an ML-DSA-44 key or signature among them), an input over 64 MiB is refused
(`TooLong` for a digest; invalid for a signature), and a digest slice of another length than
its algorithm's is `WrongLength`. Verification is a host import rather than Witchy code
because the suite must come from the one validated module, and ML-DSA in guest code would be
slow and unproved; nothing about it is authority. A module built for `witchy-1` (importing
`crypto_mldsa44_verify`) is refused by the engine's profile, not run under another algorithm.

[FIPS 204](https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.204.pdf)

**WebAuthn and OIDC verification.** The Wasmtime provider also exposes
`p256_verify` for an uncompressed 65-byte P-256 point and a DER ECDSA
signature, and `rsa_pkcs1_sha256_verify` for unsigned big-endian RSA public
key components and a PKCS#1 v1.5 signature. Both verify SHA-256 signatures
and return false for invalid encodings or mismatches. These operations
take public data and require no grant.

**Sealed HMAC.** On Wasmtime, `Secret<Seal>.hmac_sha384` authenticates up
to 1 MiB of bytes into a 48-byte destination. The key stays in the host.
The runtime checks the handle's seal right and leaves the destination
untouched on error.

**Application signing.** On Wasmtime, `Mldsa87Signer` takes a 32-byte seed
from a descriptor. `sign/ml-dsa-87/DOMAIN` grants signatures for messages
beginning with the specified domain followed by a NUL byte. Use `%20`
for spaces in the domain. `sign/tuf/targets-ml-dsa-87` grants the
TAP 21 targets signing operation over the tagged SHA-512 digest. Both
operations expose the 2592-byte public key and return 4627-byte signatures
while keeping the seed in the host.

**TLS record cryptography.** The AES-128-GCM and AES-256-GCM functions in
`import crypto` take a key, a 12-byte nonce, a five-byte record header,
input bytes and a destination. Sealing appends a 16-byte authentication
tag. Opening publishes plaintext only after authentication succeeds.
The destination must have the exact expected length. The public
`verify_tag48` operation compares two 48-byte authentication tags in
constant time. Use the record layer to manage sequence numbers, and keep
directional traffic keys and their epochs separate.
[TLS record protection](https://www.rfc-editor.org/rfc/rfc8446.html#section-5.2)

**Signing is a `Signer`, a root of `main` bound by name like a fixed secret.**

```
fn main(targets: Signer, console: Console<Write>, allocator: Allocator):
```
```toml
[secrets]
targets = { use = "sign/witchy-2/targets", from = "fd:4" }
```

- The use is `sign/witchy-2/ROLE`: the `witchy-2` suite (ML-DSA-65) for one role, 1 to 32
  lowercase letters, digits and `-`. The role is the grant's; the program cannot choose it.
  `wasm-grants` writes `--secret-slot=targets:sign/witchy-2/targets:fd:4`. A document or flag
  naming `sign/witchy-1/ROLE` is refused with what changed (`the witchy-1 suite is retired`).
  The seed's source is a descriptor, `fd:N`, only: an environment variable is inherited by
  children and visible to `ps` before the engine can clear it, so `env:` is refused.
- The slot holds exactly a 32-byte ML-DSA-65 seed (FIPS 204's ξ, the registry spec's "keys
  are stored as 32-byte seeds"), raw bytes, so in practice an `fd:` source (a variable cannot
  hold a NUL). Anything else stops the run before `main`: `secret slot NAME: not an
  ML-DSA-65 seed (32 bytes)`. The engine expands the key in the provider then and clears the
  seed it read.
- `signer.public_key(&mut key)` writes the 1952-byte public key. `signer.sign(message,
  &mut signature)` writes the 3309-byte signature of a message that begins with the role's
  domain, `witchy/v2/ROLE\0`; any other message (a `witchy/v1/` one included) is
  `Err(WrongDomain)`, a signature slice of another length `Err(WrongLength)`, a message over
  1 MiB `Err(TooLong)`. The domain is in the signed bytes (AWS-LC's ML-DSA takes no context
  string), so a signature on one role's object never verifies as another's (`witchy2_verify`).
  Both slices are checked against the program's memory before the message is read or the
  signature written; the signature is the one operation's own result, so nothing mixes
  between calls.
- Signing is hedged: AWS-LC draws the per-signature randomness from its DRBG. Two signatures of
  one message differ and both verify. The guest gets no randomness from this; `Rand` stays the
  only guest-visible draw (now also AWS-LC's DRBG, seeded from the operating system).
- A `Signer` has no operation that reads its key, and no `Secret` (a bare one included)
  narrows to it; a `Secret<Seal>` or `Secret<Reveal>` has no signing operation. The engine
  checks the same: a signer's handle refuses the byte, MAC and store operations, and a sealing
  or reading handle refuses `signer_sign` and `signer_public_key`. A grant whose use is not the
  parameter's type stops the run (`wrong use (main takes a Signer)`).

Fixtures (`tests/fixtures/crypto/`): `sha256` (FIPS 180-2 "abc" for SHA-256 and SHA-384, from
a `pure fn`, and a 32-byte slice refused for SHA-384); `mldsa65-verify`, with no secret grant,
verifies a vector made by an independent FIPS 204 implementation (Go 1.27.3 `crypto/mldsa`)
from NIST's ACVP ML-DSA-65 key-generation seed, and refuses a changed message, another role,
another key, the retired suite's sizes and a truncated or changed signature; `sign` binds that
seed as a `Signer`, whose public key is the independent implementation's (shown by its
SHA-384), signs twice (different signatures, both verifying with the public key alone) and
refuses a changed message, the wrong domain, another key and a truncated signature; the
`.refused` and `.traps` fixtures are a sealed key that tries to sign, a signer that tries to
reveal, the kinds swapped in the grant, a 31-byte seed, a role that is not plain and a grant
of the retired `sign/witchy-1/` use, and a seed from a variable (`signer-env`). The engine's unit tests check AWS-LC's key generation
against the ACVP vector's SHA-256(pk || sk) and SHA-384 against FIPS 180-4's examples.

## Grantable capabilities

```
// A library's own kind of authority (tests/fixtures/capabilities/parts/limits.wy).
pub grantable capability RateLimit:
    per_minute: U64
    strict: Bool

fn main(console: Console<Write>, limit: RateLimit):
    ...
```

A `grantable capability` is a library's own kind of authority that `main` may take, granted
by the run with values: `--grant=RateLimit.per_minute=3 --grant=RateLimit.strict=true`
(book: capabilities-narrowing.md). It is sealed: only its file builds one, and the run does
(`grantable-forge.refused.wy`).

- It carries policy data and no host authority, directly or through any field: one that
  reaches a `Net` is refused (`grantable-authority.refused.wy`), and so is the `from` form
  (`grantable-from.refused.wy`). Granting it can never be a disguised `Net` grant, and a
  new library version cannot slip host authority into it behind an unchanged `main`.
- In this first version its fields are `U64` or `Bool` (`grantable-field.refused.wy`).
- A run that grants a field the program does not ask for refuses to start; a field the
  program asks for and the run does not grant stops the run at the top of `main`, before
  any of its code ("the run grants no RateLimit.strict").
- `wasm-caps` lists it as `RateLimit (user capability)` wherever it is held, and
  `wasm-caps-diff` counts a newly required one as widening.

How it works: the parser gives each grantable type a builder in its own file that reads
each field from the run by name, and turns `main`'s parameter into a `let` built by it at
the top of `main` (src/parser/grants.rs). What reads the grants is a host capability no
source can name, and it reads data only.

## Grant documents

`witchyc wasm-grants program.wy app.grants.toml` reads the whole grant of a run as one
reviewable file, binds it to `main`'s parameters by name, and prints the engine's flags
(book: capabilities-sandbox.md, "Grant documents"; format in src/compiler/grant_document.rs):

```toml
[dirs]
data = { root = "./data", rights = ["Read"] }   # main(data: Dir<Read>)
[net]
api = ["api.example.com:443"]                    # main(api: Net)
[secrets]
store = { TOKEN = "seal" }                       # main(store: SecretStore)
signing = { use = "seal", from = "env:SIGNING_KEY" }   # main(signing: Secret<Seal>)
[user_caps]
limit = { type = "RateLimit", per_minute = 5, strict = false }
```

```
witchy-wasmtime-host program.wasm $(witchyc wasm-grants program.wy app.grants.toml)
```

Each binding is printed for review (`grant: data: Dir<Read> at /srv/app/data`). A
parameter with no entry, an entry in the wrong section, or a stated right the type does not
have is an error, and nothing is printed to run with. An entry no parameter takes is an
over-grant warning; a fixed secret's entry no parameter takes is an error ("Fixed secrets").
Relative paths are the document's directory's. Secret values come from the caller's
variables (a store's, of the same names) or the sources a fixed secret names, never from the
document or the flags. `--allocator` is always
among the flags: every checked module imports its own heap's authorizer, whether or not
`main` takes an `Allocator`.

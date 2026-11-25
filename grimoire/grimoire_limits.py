"""Manifest run limits and their translation to engine flags."""



class ProjectError(Exception):
    pass


# The byte budgets and counts a run is held to, each a `[run]` key: (engine flag, least, most,
# the engine's default, what the limit is of). A limit is never authority: each only ends a
# run, or an operation of it, sooner or later. Every one that applies to what the run is
# granted is passed to the engine as a number, the default included, and shown as a `limit:`
# line, so what bounds the run is in the review and the record, not in the engine's source.
BYTE_BUDGET_MOST = 1 << 40
RUN_LIMITS = {
    "net-bytes": ("--net-bytes", 0, BYTE_BUDGET_MOST, 1_048_576),
    "file-bytes": ("--fs-bytes", 0, BYTE_BUDGET_MOST, 1_048_576),
    "file-entries": ("--fs-entries", 0, 1_000_000, 256),
    "output-bytes": ("--output-bytes", 0, BYTE_BUDGET_MOST, 65_536),
    "input-bytes": ("--input-bytes", 0, BYTE_BUDGET_MOST, 65_536),
    "memory-pages": ("--memory-pages", 4, 32_768, 4),
    "module-bytes": ("--module-bytes", 8, 16_777_216, 65_536),
    "fuel-per-connection": ("--fuel-per-accept", 1, 1_000_000_000_000, None),
    "connection-deadline-ms": ("--connection-deadline-ms", 1, 600_000, 10_000),
    # A program run once for each connection: what one connection's instance may use of each
    # byte budget, beside the whole run's (which may then be "unlimited", `PER_CONNECTION_OWN`).
    "net-bytes-per-connection": ("--net-bytes-per-connection", 0, BYTE_BUDGET_MOST, None),
    "file-bytes-per-connection": ("--fs-bytes-per-connection", 0, BYTE_BUDGET_MOST, None),
    "file-entries-per-connection": ("--fs-entries-per-connection", 0, 1_000_000, None),
    "output-bytes-per-connection": ("--output-bytes-per-connection", 0, 268_435_456, None),
}
# A whole run's budget written "unlimited", and the key of each connection's own that must be
# set beside it: a run is never without a bound on what one piece of work may use. Only in the
# executable's own manifest (`read_limits` reads no other: a package's manifest is never a
# run's), only for a program run once for each connection, and shown on a line of its own.
PER_CONNECTION_OWN = {"net-bytes": "net-bytes-per-connection", "file-bytes": "file-bytes-per-connection",
                      "file-entries": "file-entries-per-connection", "output-bytes": "output-bytes-per-connection"}
# Standard input is a stream: `input-bytes` is a running budget, and may be "unlimited". An
# input file is read whole before the program starts, and has the engine's 16 MiB at most.
INPUT_FILE_MOST = 16_777_216
# The sizes of a module's own memory, each a `[run]` key: (least, most, the compiler's default).
# They are not the engine's to set: the compiler builds them into the module (the allocator's
# layout, the stack's place), so a module built with other sizes is another module, with another
# digest in a run's record. Passed to the compiler with the grant review, and shown as `limit:`
# lines like the rest. The ranges are the compiler's (`Heap::checked`).
BUILD_SIZES = {"heap-pages": (3, 16_384, 3), "slots": (64, 1_048_576, 64), "stack-bytes": (1024, 65_536, 16_384)}
RUN_KEYS = ("entry", "deadline-ms", "fuel", *RUN_LIMITS, *BUILD_SIZES)


def read_sizes(run):
    """`[run]` heap-pages, slots and stack-bytes as (pages, slots, stack bytes), the defaults
    where a key is absent. Out of range, or a shape the allocator cannot have, is refused."""
    sizes = []
    for key, (least, most, default) in BUILD_SIZES.items():
        value = run.get(key, default)
        if type(value) is not int or not least <= value <= most:
            raise ProjectError(f"[run].{key} is a whole number from {least} to {most}")
        sizes.append(value)
    if sizes[1] & (sizes[1] - 1):
        raise ProjectError("[run].slots is a power of two from 64 to 1048576")
    if sizes[2] % 8:
        raise ProjectError("[run].stack-bytes is a multiple of 8 from 1024 to 65536")
    return tuple(sizes)


def memory_pages_for(sizes):
    """The pages a module built with these sizes has, its program memory and the allocator's
    own beside it: what the engine must be allowed (`memory-pages`) for the module to start.
    The allocator's layout is the compiler's (`Heap` in src/compiler/allocator.rs): 64 bytes a
    slot, a byte for each 32-byte quantum of heap, and a bit for each byte of heap."""
    pages, slots, _ = sizes
    heap = pages * 65536 - 98304
    bitmap_low = (slots * 64 + 64 + heap // 32 + 7) & ~7
    return pages + -(-(bitmap_low + heap // 8) // 65536)


def read_limits(run):
    """The engine's run limits from `[run]`, as the flags that set them, the defaults included:
    how long the run may last (`deadline-ms`, 1 to 600000, or "none": a service, which ends when
    it is stopped or its launcher ends), how many instructions it may execute (`fuel`, a count,
    or "unmetered"), how many from one accepted connection to the next (`fuel-per-connection`),
    and the bytes it may move (`net-bytes`, `file-bytes`, `output-bytes`), the memory it may
    have (`memory-pages`) and the size of its module (`module-bytes`). A key the table does not
    know is refused, and so is a value out of range: never ignored, never rounded."""
    unknown = sorted(set(run) - set(RUN_KEYS))
    if unknown:
        raise ProjectError(f"invalid [run].{unknown[0]}. Expected one of: {', '.join(RUN_KEYS)}")
    limits = []
    deadline = run.get("deadline-ms", 1000)
    if deadline != "none" and (type(deadline) is not int or not 1 <= deadline <= 600_000):
        raise ProjectError('[run].deadline-ms is a number of milliseconds from 1 to 600000, or "none" for a service that runs until it is stopped')
    limits.append(f"--deadline-ms={deadline}")
    fuel = run.get("fuel", 1_000_000)
    per_connection = run.get("fuel-per-connection")
    if fuel == "unmetered":
        if per_connection is not None:
            raise ProjectError('[run].fuel-per-connection counts instructions, and [run].fuel is "unmetered": use one')
        limits.append("--unmetered")
    elif type(fuel) is not int or not 1 <= fuel <= 1_000_000_000_000:
        raise ProjectError('[run].fuel is an instruction count from 1 to 10^12, or "unmetered"')
    else:
        limits.append(f"--fuel={fuel}")
    needed = memory_pages_for(read_sizes(run))
    for key, (flag, least, most, default) in RUN_LIMITS.items():
        # The memory a run may have is, unless the manifest says more, what its module is built with.
        value = run.get(key, needed if key == "memory-pages" else default)
        if value is None:
            continue
        if value == "unlimited" and (key in PER_CONNECTION_OWN or key == "input-bytes"):
            limits.append(f"{flag}=unlimited")
            continue
        if type(value) is not int or not least <= value <= most:
            unlimited = ' or "unlimited" (for a program run once for each connection, beside [run].' + PER_CONNECTION_OWN[key] + ")" if key in PER_CONNECTION_OWN else \
                ' or "unlimited" (standard input only)' if key == "input-bytes" else ""
            raise ProjectError(f"[run].{key} is a whole number from {least} to {most}{unlimited}")
        if key == "memory-pages" and value < needed and any(size in run for size in BUILD_SIZES):
            raise ProjectError(f"[run].memory-pages = {value} is less than the {needed} pages a module built with this heap-pages and slots has "
                               f"(its memory and the allocator's own): write memory-pages = {needed}, or leave the key out")
        limits.append(f"{flag}={value}")
    return limits


def applicable_limits(limits, flags, run):
    """The limits that bound what this run is granted, in a fixed order: a budget for a grant
    the run does not have (network bytes with no listener) bounds nothing, is not passed and is
    not shown; set in the manifest all the same, it is refused, as a grant no parameter takes is."""
    def has(*prefixes):
        return any(flag == prefix or flag.startswith(prefix + "=") for flag in flags for prefix in prefixes)
    # A program run once for each connection (`main(connection: Socket)`): each instance has
    # its own count, which is `fuel-per-connection` where the manifest sets it and `fuel`
    # otherwise, passed as the engine's `--fuel`; there is no count between accepts to set.
    each = PER_CONNECTION in flags
    if each:
        if "--unmetered" in limits:
            raise ProjectError('[run].fuel is "unmetered", and a program run once for each connection has a count for each: set fuel-per-connection (or fuel)')
        own = next((limit.partition("=")[2] for limit in limits if limit.startswith("--fuel-per-accept=")), None)
        limits = [f"--fuel={own}" if own is not None and limit.startswith("--fuel=") else limit for limit in limits if not limit.startswith("--fuel-per-accept=")]
    writes = "write" in next((f.partition("=")[2] for f in flags if f.startswith("--dir-rights=")), "")
    stdin = "--input-stdin" in flags
    # "unlimited", or an input past what a file may hold: refused where it does not apply,
    # whatever the run's grants (a budget without a ceiling is never passed on by default).
    for limit in limits:
        name, _, value = limit.partition("=")
        key = next((key for key, (flag, *_) in RUN_LIMITS.items() if flag == name), None)
        if key == "input-bytes" and has("--input") and not stdin and (value == "unlimited" or int(value) > INPUT_FILE_MOST):
            raise ProjectError(f"[run].input-bytes = {value} is for standard input ([input] input = {{ stdin = true }}): an input file is read whole before the program starts, at most {INPUT_FILE_MOST} bytes")
        if value == "unlimited" and key in PER_CONNECTION_OWN:
            own = PER_CONNECTION_OWN[key]
            if not each:
                raise ProjectError(f'[run].{key} = "unlimited" is for a program run once for each connection (main takes a Socket): a program run once has a ceiling on every budget')
            if own not in run:
                raise ProjectError(f'[run].{key} = "unlimited" needs [run].{own}: with no ceiling on the run, each connection has its own')
    needs = {"--net-bytes": ("a [listen] grant", has("--listen")),
             "--net-bytes-per-connection": ("a [listen] grant taken as a Socket (a program run once for each connection)", each),
             "--fs-bytes-per-connection": ("a [dirs] grant to a program run once for each connection", each and has("--dir")),
             "--fs-entries-per-connection": ("a [dirs] grant with the right to write to a program run once for each connection", each and writes),
             "--output-bytes-per-connection": ("the output grant to a program run once for each connection", each and has("--output")),
             "--connection-deadline-ms": ("a [listen] grant taken as a Socket (a program run once for each connection)", each),
             "--fuel-per-accept": ("a [listen] grant", has("--listen")),
             "--fs-bytes": ("a [dirs] or [files] grant", has("--dir", "--file")),
             "--fs-entries": ("a [dirs] grant with the right to write", has("--dir-rights")),
             "--output-bytes": ("the output grant", has("--output")),
             "--input-bytes": ("an [input] grant", has("--input", "--input-stdin")),
             "--module-bytes": ("", True), "--memory-pages": ("", True)}
    keys = {flag: key for key, (flag, *_) in RUN_LIMITS.items()}
    kept = []
    for limit in limits:
        name = limit.partition("=")[0]
        what, applies = needs.get(name, ("", True))
        if not applies:
            if keys[name] in run:
                raise ProjectError(f"[run].{keys[name]} bounds {what}, and this run has none")
            continue
        if name == "--module-bytes" and keys[name] not in run:
            continue
        kept.append(limit)
    return kept


# The compiler's flag for `main(connection: Socket)`: the engine accepts, and runs the program
# once for each connection in a new instance.
PER_CONNECTION = "--instance-per-connection"


def limit_names(flags, run=None):
    """For each limit passed to the engine, what its operator calls it: the `[run]` key. The
    engine says so when the limit is reached (`--limit-name=FLAG=TEXT`), so its line names
    what can be changed in the manifest, not an engine flag the operator never wrote."""
    keys = {"--fuel": "fuel", "--deadline-ms": "deadline-ms"} | {flag: key for key, (flag, *_) in RUN_LIMITS.items()}
    if PER_CONNECTION in flags and "fuel-per-connection" in (run or {}):
        keys["--fuel"] = "fuel-per-connection"
    engine_names = ("--fuel", "--fuel-per-accept", "--deadline-ms", "--net-bytes", "--fs-bytes", "--fs-entries", "--output-bytes", "--input-bytes", "--connection-deadline-ms",
                    "--net-bytes-per-connection", "--fs-bytes-per-connection", "--fs-entries-per-connection", "--output-bytes-per-connection")
    passed = {flag.partition("=")[0] for flag in flags}
    return [f"--limit-name={name}=[run] {keys[name]}" for name in engine_names if name in passed]

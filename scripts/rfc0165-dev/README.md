# RFC-0165 development helpers

Throwaway-grade helpers the coverage slices were built with. They are not part
of the gate. Every script runs cargo in the background with a status file so a
tool call never sits on a 10-minute build; poll the status file instead.

Environment they assume (override with `WITCHY_DEV_TARGET`):

- `CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-inner` (warm; a fresh one costs
  a 15-25 minute cold build), `CARGO_BUILD_RUSTC_WRAPPER=""`.
- The worktree root is discovered with `git rev-parse --show-toplevel`.

| script | what it does |
|---|---|
| `build.sh` | `cargo build -p witchy` in the background; `status/build.status` holds `exit=N`; log in `status/build.log`. The binary is `$WITCHY_DEV_TARGET/debug/witchy`. |
| `one-test.sh <filter> [cargo test args]` | one `cargo test --test misc <filter>` run in the background (`status/one.status`, `status/one.log`). |
| `shards.sh` | the slice shards in sequence: launch tests, ABI catalog test, `witchy-lower` canonical tests, `witchy-wir` lib with `--features native`, `witchy-types` lib, `witchy-runtime` with `--features native` (`status/shards.log`, `status/shards.status`). |
| `census.sh` | `witchy check` on every example and `witchy parity` on the ones whose `main` takes only Console/Dir/Env; prints check and agree counts, writes `status/census-fail.txt` and `status/census-parity.txt`. |
| `queue-watch.sh <change-id>` | polls the merge-queue journal in the MAIN checkout for a terminal event on the change and writes it to `status/queue-watch.out`. |

`status/` is created next to the scripts (`scripts/rfc0165-dev/status/`) and is
listed in `.gitignore`.

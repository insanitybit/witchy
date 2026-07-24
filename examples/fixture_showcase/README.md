# Deterministic fixture showcase

This rune demonstrates Witchy's testing boundary in one small application:

- pure decision logic is tested without capabilities;
- production code receives `Console`, `Clock`, `Env`, and argv explicitly;
- the root test package declares an owned `capability FixtureWorld` record;
- the runner assembles that record from a validated fixture plan;
- exact provider call counts make unexpected effects fail the test;
- the same fixture test runs under the interpreter and Wasmtime.

Run the pure unit test:

```sh
witchy test --filter release_line examples/fixture_showcase
```

Run the capability test with deterministic providers and backend parity:

```sh
witchy test \
  --fixtures examples/fixture_showcase/release.fixture.json \
  --backend both \
  --filter fixture_world \
  --show-output \
  examples/fixture_showcase
```

The output includes:

```text
release api at 1700000000000ms in staging
```

Use `--format json` to retain the versioned transcript and per-test result in
one machine-readable document. A completed test failure exits 1; malformed
plans, unsupported authority, and infrastructure failures exit 2.

The fixture plan grants no ambient host authority. Its `Clock`, `Env`, and argv
values are deterministic, Console output is captured, and a dependency cannot
request the compiler-assembled `FixtureWorld`.

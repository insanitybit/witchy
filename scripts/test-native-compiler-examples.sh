#!/usr/bin/env bash
# Fresh inventory, dynamic targets, separate policies, and failure propagation.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
fixture="$(mktemp -d "${TMPDIR:-/tmp}/witchy-native-examples-test.XXXXXX")"
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/scripts/bin"
cp "$root/scripts/native-compiler-examples.sh" "$root/scripts/nextest-admitted.sh" \
    "$root/scripts/rfc0165-deferred-filter.sh" "$root/scripts/rfc0165-deferred-tests.txt" \
    "$root/scripts/nightly-deferred-tests.txt" "$fixture/scripts/"
git -C "$fixture" init -q
git -C "$fixture" -c user.name=Fixture -c user.email=fixture@example.invalid \
    commit -qm initial --allow-empty
cat > "$fixture/scripts/bin/cargo" <<'MOCK'
#!/usr/bin/env python3
import json, os, pathlib, sys
a = sys.argv[1:]
with open(os.environ['NATIVE_TEST_LOG'], 'a') as log:
    log.write(json.dumps(a) + '\n')
mode = os.environ.get('NATIVE_TEST_FAILURE', '')
targets = ['unit', 'structural_contracts', 'rfc0165_native_gate', 'future_contract']
if a[0] == 'metadata':
    if mode == 'metadata': sys.exit(17)
    wir = [{'name': name, 'kind': ['test'], 'test': True} for name in targets]
    wir.append({'name': 'witchy_wir', 'kind': ['lib'], 'test': mode == 'new-lib'})
    print(json.dumps({'packages': [
        {'name': 'witchy-wir', 'id': 'wir-id', 'targets': wir},
        {'name': 'witchy-lower', 'id': 'lower-id', 'targets': [
            {'name': 'codegen', 'kind': ['test'], 'test': True},
            {'name': 'new_lower_contract', 'kind': ['test'], 'test': True}]},
        {'name': 'witchy', 'id': 'root-id', 'targets': [
            {'name': 'witchy', 'kind': ['bin'], 'test': True}]}]}))
elif a[:2] == ['nextest', 'list']:
    if mode == 'build': sys.exit(18)
    rows = [('wir-id', 'test', name) for name in targets] + [('root-id', 'bin', 'witchy')]
    if 'witchy-lower' in a:
        rows += [('lower-id', 'test', name) for name in ['codegen', 'new_lower_contract']]
    if mode == 'missing-lower': rows = [r for r in rows if r[2] != 'new_lower_contract']
    if mode == 'missing': rows.pop(0)
    if mode == 'wrong-package': rows[0] = ('root-id', 'test', 'unit')
    print(json.dumps({'rust-binaries': {name: {'package-id': p, 'kind': kind,
        'binary-name': name, 'binary-path': '/fixture/' + name} for p, kind, name in rows}}))
elif a[:2] == ['nextest', 'run']:
    for flag in ['--cargo-metadata', '--binaries-metadata']:
        json.loads(pathlib.Path(a[a.index(flag) + 1]).read_text())
    expr = a[a.index('-E') + 1]
    stage = 'wir' if 'package(=witchy-wir)' in expr else 'examples'
    if mode == stage: sys.exit(19)
    if mode == 'head-change' and stage == 'examples':
        import subprocess
        subprocess.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
            'commit', '-qm', 'changed', '--allow-empty'], check=True)
else:
    raise AssertionError(a)
MOCK
chmod +x "$fixture/scripts/bin/cargo"
export NATIVE_TEST_LOG="$fixture/commands.jsonl"
for scope in slice full mixed; do
    partition=full
    extra=()
    [ "$scope" != slice ] || partition=hash:1/8
    [ "$scope" != mixed ] || extra=(witchy-lower)
    : > "$NATIVE_TEST_LOG"
    bash "$fixture/scripts/native-compiler-examples.sh" "$partition" ${extra[0]+"${extra[@]}"}
    python3 - "$NATIVE_TEST_LOG" "$partition" "$scope" <<'PY'
import json, pathlib, sys
rows = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
assert len(rows) == 4, rows
assert rows[0][0] == 'metadata' and rows[1][:2] == ['nextest', 'list']
assert '--lib' not in rows[1] and '--tests' not in rows[1]
mixed = sys.argv[3] == 'mixed'
assert rows[1].count('-p') == (3 if mixed else 2)
for name in ['unit', 'structural_contracts', 'rfc0165_native_gate', 'future_contract']:
    assert ['--test', name] in [rows[1][i:i+2] for i in range(len(rows[1]) - 1)]
compiler_expr = rows[2][rows[2].index('-E') + 1]
if mixed:
    assert 'new_lower_contract' in rows[1]
    assert compiler_expr.startswith('(package(=witchy-wir)) or (package(=witchy-lower) and (')
    assert 'not' in compiler_expr, 'lower admitted/deferred policy must remain present'
else:
    assert compiler_expr == 'package(=witchy-wir)'
assert rows[1][rows[1].index('--features') + 1] == 'witchy-wir/native'
assert '--partition' not in rows[2]
for row in rows[2:]:
    assert '--ignore-default-filter' not in row
    assert not any(flag in row for flag in ['-p', '--features', '--bin', '--test', '--no-default-features'])
expr = rows[3][rows[3].index('-E') + 1]
assert expr.startswith('package(=witchy) and kind(bin) and binary(=witchy) and test(/^example_tests::/) and (')
assert 'not' in expr, 'admitted/deferred policy must remain present'
assert ('--partition' in rows[3]) == (sys.argv[2] != 'full')
if sys.argv[2] != 'full': assert rows[3][rows[3].index('--partition') + 1] == sys.argv[2]
assert not pathlib.Path(rows[2][rows[2].index('--binaries-metadata') + 1]).exists()
PY
done
for failure in metadata new-lib build missing missing-lower wrong-package wir examples head-change; do
    if NATIVE_TEST_FAILURE="$failure" bash "$fixture/scripts/native-compiler-examples.sh" full witchy-lower \
        > "$fixture/failure.log" 2>&1; then
        echo "native compiler examples accepted $failure failure" >&2; exit 1
    fi
done
echo 'native compiler examples: shared build, complete inventory, policies and failures passed'

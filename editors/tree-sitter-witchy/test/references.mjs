// The editor parser is an ordinary-source surface, never a migration parser.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const root = new URL('..', import.meta.url).pathname;
const cli = join(root, 'node_modules/.bin/tree-sitter');
const directory = mkdtempSync(join(tmpdir(), 'witchy-editor-syntax-'));
let checks = 0;
try {
  const valid = [
    "fn read(value: &String) -> &String:\n    value\n",
    "fn first(items: &[Int]) -> &Int:\n    &items[0]\n",
    "type Pair(a):\n    shared: &a\n    items: &[a]\n",
    "fn view(var value: String):\n    let borrowed = &value\n    borrowed\n",
    "fn named_mut():\n    var mut = 1\n    mut = mut + 1\n    &mutant\n",
    "fn edit(var value: String):\n    value = \"done\"\n",
    "fn qualify(let value: frozen String, own other: unique String):\n    ()\n",
    "type Views(a):\n    value: &a\n\nfn nominal(value: Views(Int)):\n    ()\n",
    "type Types = (Views(Int), m.Views(Int))\n",
    "fn generated():\n    quote type:\n        &[Int]\n",
    "fn generated():\n    quote item:\n        fn read(value: &Int):\n            ()\n",
    "fn unit(value: ()) -> ():\n    match value:\n        () -> ()\n",
    "fn leading(value: Int):\n    let before = value\n    -value\n    &value\n    *value\n",
    "fn continued(value: Int):\n    let result = (value\n        * value\n        - value\n        & value)\n",
    "fn conjunction():\n    let both = true\n    && false\n",
  ];
  const invalid = [
    "fn old(value: &'a String) -> &'a String:\n    value\n",
    "fn old(value: &'a mut String) -> &'a mut String:\n    value\n",
    "type Old(a, 'owner):\n    shared: &'owner a\n",
    "fn old(value: &mut String):\n    ()\n",
    "fn old(value: String):\n    let exclusive = &mut value\n    ()\n",
    "fn old(value: String):\n    let exclusive = & mut value\n    ()\n",
    "fn old():\n    var mut = 1\n    &mut\n",
    "fn old(value: &'a mut String):\n    *value = \"done\"\n",
    "type Old('a):\n    value: &'a String\n",
    "fn old(value: View(String, 'a)):\n    ()\n",
    "fn old(value: let('a) String):\n    ()\n",
    "fn old(let('a) value: String):\n    ()\n",
    "fn old(var('a) value: String):\n    ()\n",
    "fn old(value: local unique String):\n    ()\n",
    "type Old = List(View(String, 'a))\n",
    "type Old = fn(var('a) String) -> String\n",
    "fn generated():\n    quote type:\n        &'a mut String\n",
    "fn generated():\n    quote item:\n        fn old(value: &'a mut String):\n            ()\n",
    "fn generated():\n    quote type:\n        View(String, 'a)\n",
    "fn generated():\n    quote type:\n        local unique String\n",
    "fn generated():\n    quote item:\n        fn edit(var('a) value: String):\n            ()\n",
  ];
  for (const [expected, sources] of [[0, valid], [1, invalid]]) {
    for (const source of sources) {
      for (const prefix of ['', 'mode opt\n\n']) {
        const path = join(directory, `case-${checks}.witchy`);
        writeFileSync(path, prefix + source);
        const parsed = spawnSync(cli, ['parse', path], { cwd: root, encoding: 'utf8' });
        assert.equal(parsed.status, expected, `${source}\n${parsed.stdout}\n${parsed.stderr}`);
        if (expected === 1) assert.match(parsed.stdout, /ERROR|MISSING/, parsed.stderr);
        if (source.startsWith('fn leading')) {
          assert.equal((parsed.stdout.match(/unary_expression/g) || []).length, 3, parsed.stdout);
          assert.ok(!parsed.stdout.includes('binary_expression'), parsed.stdout);
        }
        if (source.startsWith('fn continued')) {
          assert.equal((parsed.stdout.match(/binary_expression/g) || []).length, 3, parsed.stdout);
        }
        if (expected === 0) {
          const query = spawnSync(cli, ['query', 'queries/highlights.scm', path], { cwd: root, encoding: 'utf8' });
          assert.equal(query.status, 0, query.stderr);
          if (source.includes('items: &[Int]')) assert.match(query.stdout, /type.*Int/);
        }
        checks++;
      }
    }
  }
  console.log(`${checks} canonical/retired syntax checks passed; highlight queries compiled.`);
} finally {
  rmSync(directory, { recursive: true });
}

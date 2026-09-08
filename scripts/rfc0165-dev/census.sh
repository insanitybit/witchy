#!/usr/bin/env bash
# Census: check every example; parity for the runnable ones (Console/Dir/Env mains).
source "$(dirname "$0")/common.sh"
ok=0; fail=0; : > "$STATUS/census-fail.txt"; : > "$STATUS/census-parity.txt"
for f in examples/*/src/*.witchy; do
  if out=$("$WITCHY_BIN" check "$f" 2>&1); then
    ok=$((ok+1))
    if grep -qE "^fn main\(console: Console(, [a-z_]+: (Dir(\[[A-Za-z, ]*\])?|Env))*\)" "$f"; then
      if pout=$(timeout 120 "$WITCHY_BIN" parity "$f" 2>&1); then echo "AGREE $f" >> "$STATUS/census-parity.txt"
      else echo "DISAGREE $f :: $(echo "$pout" | grep -v warning | tail -1 | cut -c1-160)" >> "$STATUS/census-parity.txt"; fi
    fi
  else
    fail=$((fail+1)); echo "$f :: $(echo "$out" | grep -v warning | head -1 | sed 's/^[^:]*: //' | cut -c1-200)" >> "$STATUS/census-fail.txt"
  fi
done
echo "check ok=$ok fail=$fail"
echo "runnable agree: $(grep -c ^AGREE "$STATUS/census-parity.txt")  disagree: $(grep -c ^DISAGREE "$STATUS/census-parity.txt")"
grep ^DISAGREE "$STATUS/census-parity.txt"

#!/usr/bin/env python3
"""Classify nextest failures by refusal signature (RFC-0165 landing ledger)."""
import re, sys, collections
log=open(sys.argv[1], errors='replace').read()
start=log.find('Summary [')
tail=log[start:] if start>=0 else log
entries=[]
current=None
for line in tail.splitlines():
    m=re.match(r'^\s*(?:TRY \d+ )?(FAIL|TIMEOUT|SIGSEGV|ABORT|SIGABRT)\s+\[[^\]]*\]\s*\(\s*\d+/\d+\)\s+(\S+)\s+(\S+)', line)
    if m:
        current=[m.group(1), m.group(2), m.group(3), []]; entries.append(current); continue
    if current is not None: current[3].append(line)
policy=[('unsupported canonical v1 layout type', r'unsupported canonical v1 layout type'),
        ('unsupported lowering (other)', r'unsupported lowering'),
        ('canonical SafeWIR refusal', r'canonical SafeWIR (?:pipeline|lowering) failed'),
        ('missing checked source witness', r'MissingCheckedSourceWitness'),
        ('unsupported canonical obligation', r'UnsupportedCanonicalObligation'),
        ('cannot compile to WASM', r'cannot compile to WASM'),
        ('Command failed (wrapped refusal)', r'Command failed'),
       ]
summary=collections.Counter(); other=[]; bybin=collections.Counter(); percls_bin=collections.defaultdict(collections.Counter)
seen=set()
for kind,binary,test,lines in entries:
    if (binary,test) in seen: continue
    seen.add((binary,test)); bybin[binary]+=1
    text='\n'.join(lines)
    cls=None
    for name,pat in policy:
        if re.search(pat, text): cls=name; break
    if cls is None:
        cls='OTHER' if kind=='FAIL' else kind
        other.append((binary,test,[l for l in lines if l.strip()][-14:]))
    summary[cls]+=1; percls_bin[cls][binary]+=1
print("unique failing tests:", len(seen))
for k,v in summary.most_common(): print(f"  {v:4d}  {k}")
print("\nby binary:")
for b,c in bybin.most_common(): print(f"  {c:4d}  {b}")
print("\nOTHER / timeouts (no policy refusal signature):", len(other))
for b,t,lines in other:
    print(f"\n## {b} {t}")
    for l in lines: print("   ", l.strip()[:220])

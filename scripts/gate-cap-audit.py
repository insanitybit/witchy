#!/usr/bin/env python3
"""Compare every test's nextest kill-ceiling against its OBSERVED runtime.

Two failure modes cost this repo weeks of false gate reds, and both are
computable from gate logs that were already on disk:

  A. A cap set below the p99 of the test's own SUCCESSFUL runs. Such a test
     does not fail when it breaks -- it fails at random, forever, and reds a
     whole serialized gate each time (--max-fail=1:immediate).

  B. A filter that silently stops matching. Anchored `^example_tests::(...)$`
     patterns died the day those tests moved into submodules; the override
     stayed in the file, looking like coverage, applying to nothing.

Durations come from COMPLETED runs only (PASS/SLOW/FLAKY). Including killed
runs would be circular: a killed test reports exactly its cap, so every
timeout inflates its own p99 to the ceiling and the check silently passes.

Usage: scripts/gate-cap-audit.py
"""
import re, glob, tomllib, collections, os, time
ANSI=re.compile(r'\x1b\[[0-9;]*[A-Za-z]')
LINE=re.compile(r'^\s*(?:TRY \d+ )?(PASS|SLOW|FLAKY|FAIL|TIMEOUT|TMT)\s*\[[>\s]*([0-9.]+)s\]\s*\(\s*[\d─/]+\)\s*(\S+)\s+(\S+)\s*$')
# Only consider logs from the last N days. Test names change -- a suite
# reorganised into submodules leaves its old names in months of history -- and
# a name that no longer exists cannot be under-capped. Without this the "dead
# filter" check reports long-renamed tests forever.
RECENT_DAYS = int(os.environ.get("GATE_CAP_AUDIT_DAYS", "10"))
_cutoff = time.time() - RECENT_DAYS * 86400
ok=collections.defaultdict(list); killed=collections.Counter()
for f in glob.glob("state/merge-queue/logs/*.log"):
    try:
        if os.path.getmtime(f) < _cutoff: continue
    except OSError: continue
    try: fh=open(f,errors="ignore")
    except OSError: continue
    with fh:
        for line in fh:
            m=LINE.match(ANSI.sub("",line.rstrip()))
            if not m: continue
            kind,d,binid,test=m.group(1),float(m.group(2)),m.group(3),m.group(4)
            if kind in ("TIMEOUT","TMT"): killed[(binid,test)]+=1
            elif kind in ("PASS","SLOW","FLAKY"): ok[(binid,test)].append(d)
cfg=tomllib.load(open(".config/nextest.toml","rb")); prof=cfg["profile"]["default"]
def cap(st):
    if not st: return None
    return float(str(st.get("period","0s")).rstrip("s"))*int(st.get("terminate-after",1))
dflt=cap(prof.get("slow-timeout"))
ovs=[(o.get("priority",0),o.get("filter",""),cap(o.get("slow-timeout"))) for o in prof.get("overrides",[]) if cap(o.get("slow-timeout"))]
ovs.sort(key=lambda x:-x[0])
def match(filt,binid,test):
    for m in re.finditer(r'test\(/(.+?)/\)',filt):
        try:
            if re.search(m.group(1),test): return True
        except re.error: pass
    for m in re.finditer(r'binary_id\(([^)]+)\)',filt):
        if m.group(1).strip()==binid and '&' not in filt: return True
    return False
def eff(binid,test):
    for pri,filt,c in ovs:
        if match(filt,binid,test): return c,filt
    return dflt,"<default>"

print("== A. caps below the p99 of SUCCESSFUL runs (will kill healthy runs) ==")
rows=[]
for k,ds in ok.items():
    if len(ds)<8: continue
    ds=sorted(ds); p99=ds[min(len(ds)-1,int(0.99*len(ds)))]
    c,_=eff(*k); h=c/max(p99,0.01)
    if h<1.6: rows.append((h,c,p99,ds[-1],len(ds),killed[k],k[1]))
rows.sort()
for h,c,p99,mx,n,kl,t in rows[:14]:
    print(f"  {h:4.2f}x cap={c:5.0f}s p99={p99:6.1f}s max={mx:6.1f}s runs={n:4d} kills={kl:4d}  {t[:52]}")
print(f"  ({len(rows)} tests under 1.6x headroom)")

print("\n== B. named in an override's regex but NOT matched by it (dead filter) ==")
found=0
for pri,filt,c in ovs:
    names=set(re.findall(r'[a-z][a-z0-9_]{12,}',filt))
    for (binid,test) in list(ok)+list(killed):
        leaf=test.rsplit("::",1)[-1]
        if leaf in names and not match(filt,binid,test):
            print(f"  cap={c:.0f}s NOT applied to {test[:62]}"); found+=1
            if found>10: break
    if found>10: break
if not found: print("  (none)")

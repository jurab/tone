#!/usr/bin/env python3
"""Per-segment pitch-track evaluator. Runs the analyzer (CSV mode) on a file
with given env params, splits voiced frames into note segments, and reports
each segment's dominant note + how many frames stray (octave or >0.5 semitone).
Usage: eval.py <wav> [env KEY=VAL ...]"""
import sys, os, subprocess, csv, math
from collections import Counter

wav = sys.argv[1]
env = dict(os.environ, CSV="1")
gate = "0.008"
for a in sys.argv[2:]:
    if a.replace('.','').isdigit(): gate = a
    elif "=" in a:
        k, v = a.split("=", 1); env[k] = v

out = subprocess.run(["../target/release/analyze", wav, gate],
                     capture_output=True, text=True, env=env).stdout
rows = [(float(r["t"]), float(r["midi"]) if r["midi"] else None)
        for r in csv.DictReader(out.splitlines())]

def note(m):
    n=["C","C#","D","D#","E","F","F#","G","G#","A","A#","B"]
    mr=int(round(m)); return f"{n[((mr%12)+12)%12]}{mr//12-1}"

# split into segments on unvoiced gaps
segs=[]; cur=[]
for t,m in rows:
    if m is None:
        if len(cur)>=3: segs.append(cur)
        cur=[]
    else: cur.append((t,m))
if len(cur)>=3: segs.append(cur)

print(f"\n## {os.path.basename(wav)}  [{ ' '.join(a for a in sys.argv[2:] if '=' in a) or 'defaults'}]  {len(segs)} segments")
TOT_OCT=TOT_OFF=TOT_N=0
for s in segs:
    ms=sorted(m for _,m in s)
    med=ms[len(ms)//2]
    nm=note(med)
    off=sum(1 for _,m in s if abs(m-med)>0.5)
    octs=sum(1 for _,m in s if abs(abs(m-med)-12)<2)
    spread=max(ms)-min(ms)
    flag = " <-- DIRTY" if off>len(s)*0.1 or octs>0 else ""
    print(f"  {s[0][0]:5.1f}-{s[-1][0]:5.1f}s  {nm:4} ({med:5.2f})  n={len(s):3}  off>0.5st={off:3} oct={octs:2} spread={spread:4.1f}st{flag}")
    TOT_OCT+=octs; TOT_OFF+=off; TOT_N+=len(s)
print(f"  TOTAL: n={TOT_N} oct={TOT_OCT} off>0.5st={TOT_OFF}")

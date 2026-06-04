#!/usr/bin/env python3
"""Trustworthy parameter sweep: runs the analyzer over every test file for each
param combo and tabulates octave + off-pitch errors. No bash pipelines."""
import os, subprocess, csv, itertools

FILES = [  # (file, gate, [expected midi per note group] for sanity)
    ("e3_ref_48k_mono.wav", "0.008", "E3 decay (oct-down torture)"),
    ("c4d2_ref_48k_mono.wav", "0.008", "D4 then D2 (oct-up torture)"),
    ("synth_sine_e3.wav", "0.001", "sine E3"),
    ("synth_saw_e3.wav", "0.001", "saw E3"),
    ("synth_square_e2.wav", "0.001", "square E2 (low, strong harmonics)"),
    ("synth_square_a3.wav", "0.001", "square A3"),
    ("synth_missing_f_e3.wav", "0.001", "missing-fundamental E3"),
]

def run(wav, gate, env_over):
    env = dict(os.environ, CSV="1", **env_over)
    out = subprocess.run(["../target/release/analyze", wav, gate],
                         capture_output=True, text=True, env=env).stdout
    rows = [(float(r["t"]), float(r["midi"]) if r["midi"] else None)
            for r in csv.DictReader(out.splitlines())]
    # segment on unvoiced gaps
    segs=[]; cur=[]
    for t,m in rows:
        if m is None:
            if len(cur)>=3: segs.append(cur)
            cur=[]
        else: cur.append(m)
    if len(cur)>=3: segs.append(cur)
    oct_err=off_err=n=0
    for s in segs:
        ms=sorted(s); med=ms[len(ms)//2]
        for m in s:
            n+=1
            if abs(m-med)>0.5: off_err+=1
            if abs(abs(m-med)-12)<2: oct_err+=1
    return oct_err, off_err, n

import sys
# param combos from CLI: WINDOW=a,b KSUB=x,y CLAR=...
grid={}
for a in sys.argv[1:]:
    k,v=a.split("="); grid[k]=v.split(",")
keys=list(grid)
combos=[dict(zip(keys,vals)) for vals in itertools.product(*grid.values())] or [{}]

for combo in combos:
    print(f"\n### {combo or 'baked defaults'}")
    tot_oct=tot_off=tot_n=0
    for wav,gate,desc in FILES:
        o,f,n=run(wav,gate,combo)
        tot_oct+=o; tot_off+=f; tot_n+=n
        mark = "  ***" if o>0 else ""
        print(f"   {desc:38} oct={o:4} off={f:4} n={n:4}{mark}")
    print(f"   {'>>> TOTAL':38} oct={tot_oct:4} off={tot_off:4} n={tot_n:4}")

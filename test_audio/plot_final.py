#!/usr/bin/env python3
"""Render the production detector's pitch track for one or more recordings on a
chromatic grid (mimics the app trace) so residual artifacts are visible."""
import csv, subprocess, sys, os
import matplotlib; matplotlib.use("Agg")
import matplotlib.pyplot as plt

FILES = [("c4d2_ref_48k_mono.wav","0.008","D4 x4  then  D2 x4"),
         ("e3_ref_48k_mono.wav","0.008","E3 x3 (decaying)")]
def track(wav,gate):
    out=subprocess.run(["../target/release/analyze",wav,gate],capture_output=True,text=True,
                       env=dict(os.environ,CSV="1",CLAR=os.environ.get("CLAR","0.7"))).stdout
    ts,ms=[],[]
    for r in csv.DictReader(out.splitlines()):
        ts.append(float(r["t"])); ms.append(float(r["midi"]) if r["midi"] else None)
    return ts,ms
def note(m):
    n=["C","C#","D","D#","E","F","F#","G","G#","A","A#","B"]; return f"{n[m%12]}{m//12-1}"

fig,axes=plt.subplots(len(FILES),1,figsize=(13,7))
lo,hi=36,64
for ax,(wav,g,title) in zip(axes,FILES):
    ts,ms=track(wav,g)
    ax.set_facecolor("#0b0b0c")
    for m in range(lo,hi+1):
        ax.axhline(m,color="#2a2a30" if m%12==0 else "#15151a",lw=1,zorder=0)
        if m%12 in (0,2,4,7): ax.text(ts[-1]*1.005,m,note(m),color="#666",fontsize=7,va="center")
    seg_t,seg_m=[],[]
    for t,m in zip(ts,ms):
        if m is None:
            if len(seg_t)>1: ax.plot(seg_t,seg_m,color="#88eeff",lw=1.5)
            seg_t,seg_m=[],[]
        else: seg_t.append(t); seg_m.append(m)
    if len(seg_t)>1: ax.plot(seg_t,seg_m,color="#88eeff",lw=1.5)
    ax.set_title(f"{title}",color="#ccc",fontsize=11,loc="left")
    ax.set_ylim(lo-.5,hi+.5); ax.set_xlim(0,ts[-1]*1.02)
    ax.set_yticks([38,50,52,62]); ax.set_yticklabels(["D2","D3","E3","D4"],color="#999")
    ax.tick_params(colors="#666")
axes[-1].set_xlabel("time (s)",color="#999")
fig.patch.set_facecolor("#0b0b0c"); fig.tight_layout()
fig.savefig("final_tracks.png",dpi=110,facecolor="#0b0b0c")
print("wrote",os.path.abspath("final_tracks.png"))

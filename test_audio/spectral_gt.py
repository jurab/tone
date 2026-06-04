#!/usr/bin/env python3
"""High-resolution spectral ground truth: for each loud segment, find the
fundamental by harmonic-comb scoring (robust to missing/weak fundamental)."""
import sys, numpy as np
from scipy.io import wavfile
sr, x = wavfile.read(sys.argv[1])
if x.dtype != np.float32: x = x.astype(np.float64)/np.iinfo(x.dtype).max
x = x.astype(np.float64)

def note(hz):
    if hz<=0: return "-"
    m=69+12*np.log2(hz/440); mr=int(round(m))
    names=["C","C#","D","D#","E","F","F#","G","G#","A","A#","B"]
    return f"{names[((mr%12)+12)%12]}{mr//12-1}{int(round((m-mr)*100)):+d}c"

# segment by RMS envelope into voiced runs
win=2048; hop=512
rms=np.array([np.sqrt(np.mean(x[s:s+win]**2)) for s in range(0,len(x)-win,hop)])
gate=0.006
voiced = rms>gate
# find runs
runs=[]; i=0
while i<len(voiced):
    if voiced[i]:
        j=i
        while j<len(voiced) and voiced[j]: j+=1
        if j-i>=4: runs.append((i*hop, j*hop))
        i=j
    else: i+=1

def fundamental(seg):
    # take a strong window, zero-pad FFT, harmonic-comb score 50..400 Hz
    w = seg*np.hanning(len(seg))
    N=1<<int(np.ceil(np.log2(len(w)*4)))
    spec=np.abs(np.fft.rfft(w,n=N)); fr=np.fft.rfftfreq(N,1/sr)
    best_f,best_s=0,-1
    for f0 in np.arange(50,400,0.25):
        s=0
        for h in range(1,7):
            k=int(round(h*f0*N/sr))
            if k<len(spec): s+=spec[k]
        if s>best_s: best_s,best_f=s,f0
    return best_f

print(f"sr={sr} dur={len(x)/sr:.2f}s  found {len(runs)} voiced segments (>4 frames @ gate {gate}):")
for (a,b) in runs:
    seg=x[a:b]
    # use the loudest 0.3s window inside the run
    L=min(len(seg), int(0.3*sr))
    bestpow=0; bi=0
    for s in range(0,len(seg)-L,L//2 if L>1 else 1):
        p=np.sum(seg[s:s+L]**2)
        if p>bestpow: bestpow,bi=p,s
    f0=fundamental(seg[bi:bi+L])
    print(f"  {a/sr:5.2f}-{b/sr:5.2f}s  rms~{np.sqrt(np.mean(seg**2)):.4f}  f0={f0:6.2f}Hz  {note(f0)}")

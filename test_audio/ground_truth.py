#!/usr/bin/env python3
"""Independent ground-truth pitch analysis of e3_ref recording.
Uses FFT spectral analysis + autocorrelation, NOT the app's YIN, so it's an
independent reference for grading the Rust detector."""
import sys
import numpy as np
from scipy.io import wavfile

sr, data = wavfile.read(sys.argv[1] if len(sys.argv) > 1 else "e3_ref_48k_mono.wav")
if data.dtype != np.float32:
    data = data.astype(np.float32) / np.iinfo(data.dtype).max
x = data.astype(np.float64)
print(f"sr={sr} n={len(x)} dur={len(x)/sr:.3f}s peak={np.max(np.abs(x)):.4f}")

def hz_to_note(hz):
    if hz <= 0: return "-"
    m = 69 + 12*np.log2(hz/440.0)
    names=["C","C#","D","D#","E","F","F#","G","G#","A","A#","B"]
    mr=int(round(m)); pc=((mr%12)+12)%12; octv=mr//12-1
    cents=int(round((m-mr)*100))
    return f"{names[pc]}{octv}{cents:+d}c"

E3=164.8138
print(f"target E3 = {E3:.2f} Hz (midi 52)")

# Frame-by-frame using FFT-based harmonic product spectrum (independent of YIN)
FRAME=4096; HOP=1024
def hps_pitch(frame):
    w=frame*np.hanning(len(frame))
    rms=np.sqrt(np.mean(frame**2))
    if rms<1e-4: return 0.0,rms
    spec=np.abs(np.fft.rfft(w, n=FRAME*4))
    freqs=np.fft.rfftfreq(FRAME*4, 1/sr)
    # harmonic product spectrum, 5 harmonics
    hps=spec.copy()
    for h in range(2,6):
        dec=spec[::h]
        hps[:len(dec)]*=dec
    lo=np.searchsorted(freqs,70); hi=np.searchsorted(freqs,1200)
    pk=lo+np.argmax(hps[lo:hi])
    # parabolic refine on linear spec near pk
    return freqs[pk],rms

# also a clean autocorrelation pitch per frame for cross-check
def acf_pitch(frame):
    rms=np.sqrt(np.mean(frame**2))
    if rms<1e-4: return 0.0
    f=frame-np.mean(frame)
    corr=np.correlate(f,f,'full')[len(f)-1:]
    minlag=int(sr/1200); maxlag=int(sr/70)
    seg=corr[minlag:maxlag]
    # find first peak after the zero-lag descent
    d=np.diff(seg)
    # first index where slope goes + then we find local max
    peak=None
    for i in range(1,len(seg)-1):
        if seg[i]>seg[i-1] and seg[i]>=seg[i+1] and seg[i]>0.3*corr[0]:
            peak=i+minlag; break
    if peak is None: return 0.0
    return sr/peak

pts=[]
for s in range(0,len(x)-FRAME,HOP):
    fr=x[s:s+FRAME]
    hps_f,rms=hps_pitch(fr)
    acf_f=acf_pitch(fr)
    t=s/sr
    pts.append((t,hps_f,acf_f,rms))

# summary: only voiced frames
voiced=[(t,h,a,r) for (t,h,a,r) in pts if r>1e-3 and h>0]
print(f"\nframes total={len(pts)} voiced={len(voiced)}")
hps_hz=np.array([h for (_,h,_,_) in voiced])
acf_hz=np.array([a for (_,_,a,_) in voiced if a>0])
print(f"HPS  median={np.median(hps_hz):.2f}Hz ({hz_to_note(np.median(hps_hz))})  "
      f"p10={np.percentile(hps_hz,10):.1f} p90={np.percentile(hps_hz,90):.1f}")
print(f"ACF  median={np.median(acf_hz):.2f}Hz ({hz_to_note(np.median(acf_hz))})  "
      f"p10={np.percentile(acf_hz,10):.1f} p90={np.percentile(acf_hz,90):.1f}")

# distribution of HPS notes to see if octave doubling exists in ground truth
from collections import Counter
notes=Counter(hz_to_note(h)[:2].rstrip("+-0123456789c") + str(int(round(69+12*np.log2(h/440)))//12-1 if h>0 else 0) for h in hps_hz)
print("\nHPS note histogram:")
for note,cnt in sorted(notes.items(), key=lambda kv:-kv[1])[:8]:
    print(f"  {note}: {cnt} ({100*cnt/len(hps_hz):.0f}%)")

# timeline sketch (every ~0.25s) of HPS hz
print("\ntimeline (t: HPS_hz ACF_hz rms):")
for (t,h,a,r) in pts[::max(1,len(pts)//40)]:
    bar = hz_to_note(h) if r>1e-3 else "silence"
    print(f"  {t:5.2f}s  hps={h:7.1f} acf={a:7.1f} rms={r:.4f}  {bar}")

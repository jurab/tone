# 2026-06-04 — octave-error audit & NSDF rewrite

## What we did

Killed the octave-jump artifacts. The detector was rewritten from the original
autocorrelation-YIN (+ ad-hoc octave hack) to the **McLeod Pitch Method (NSDF)**
with structural octave correction and a small display-side median filter. Pitch
now holds the correct octave from D2 (~73 Hz) up through D4 (~293 Hz), validated
on real recordings and synthetic torture tones.

Also split the DSP into a library (`src/lib.rs`) shared by the GUI and a new
offline analyzer binary (`src/bin/analyze.rs`), and built a Python validation
harness under `test_audio/` so detector changes can be graded objectively
instead of by eyeballing the live trace.

## The bug (audit findings)

Measured the original detector against a sustained-E3 recording: **34% of voiced
frames jumped an octave DOWN to E2** (206 jumps).

- **The "octave correction" hack was backwards.** It flipped `τ → 2·τ` (an
  octave *down*) whenever the period-doubled dip looked ~20% deeper. On a
  decaying string the doubled-period match routinely gets deeper, so it dragged
  E3 → E2 even on loud, high-confidence frames. It wasn't a knife-edge to
  re-tune; it was the wrong operation. Deleted.
- **The YIN absolute-threshold selection itself octave-drops** on strong-harmonic
  decaying tones: when the fundamental dip rises above the fixed threshold while
  the 2·τ dip stays below it, "first dip below threshold" skips the fundamental.
- A CMNDF cumulative-sum bug (omitted lags below `min_lag`) — real but measured
  to contribute ~nothing here.

## Decisions

- **NSDF / McLeod, not YIN.** NSDF (normalized square difference, range [-1,1])
  gives a clean periodicity score and its peak height doubles as voicing
  confidence — no magic energy constant.
- **A single global octave threshold cannot work.** A decaying E3 needs an
  *aggressive* shortest-period preference (its true peak sits ~0.85× a taller
  octave-down peak); a D2 needs a *conservative* one (its 2nd-harmonic peak sits
  ~0.7× and a low threshold climbs to it). They only separate once the analysis
  window is long enough to sharpen the NSDF.
- **Final selection:** take the tallest NSDF peak, then climb to a shorter
  integer sub-multiple only if that sub-period scores ≥ `K_SUB` (0.88) of it.
  A genuine sub-octave qualifies; a mere harmonic doesn't. This fixes
  octave-DOWN without inducing octave-UP, with no temporal state.
- **Window = 4096 (85 ms).** 8192 was octave-cleaner but doubled onset latency
  and the silence→note window-fill smear; 2048 was too short (octaves returned).
  4096 is the knee.
- **`MIN_HZ` = 60.** A lower floor admits spurious NSDF peaks at high harmonic
  multiples of mid/high notes that outrank the fundamental (D4's tallest peak is
  its 5th harmonic).
- **Display median-5 + clarity gate (0.85) + onset gate.** A 5-frame median on
  the drawn track removes the 1–2-frame octave/onset spikes while preserving
  vibrato/slides. Clarity + onset gates drop attack transients and weak tails.
- **Dropped the bootstrap "raw measurement / no temporal smoothing" constraint**
  at the user's request — the median filter is the standard, correct fix and the
  raw-only stance was costing artifact-freeness for no real benefit here.
- **Default RMS gate bumped to −42 dB** (was −60). The NSDF clarity floor now
  does the "is this pitched" job the RMS gate used to overreach on.

## Validation

Offline harness (`test_audio/`, exact GUI window/cadence/filter): **0 octave
errors** on the E3-decay recording, the D4+D2 recording, and all synthetic tones
(sine / saw / square / **missing-fundamental** → correct). Only residual is a
brief (~85 ms) pitch wobble at note attacks (window-fill), cosmetic and at note
edges. Rendered before/after plots in `test_audio/` show the original E3↔E2
sawtooth vs the clean flat lines.

## Prior-art check (FMIT)

Cloned `gillesdegottex/fmit`. Its default estimator (`CombedFT`) is a *spectral*
harmonic-comb/cepstral method, and its octave decision is a tuned **sub-harmonic
audibility ratio** (default 0.1), exposed as a UI knob. Lesson: octave
disambiguation is inherently a tuned threshold even in a mature tuner — our
`K_SUB` is the time-domain twin of FMIT's `audib_ratio`. No shortcut was missed,
so we did **not** port FMIT's FFT engine (new dependency, its own knob) since the
NSDF version already achieves zero octave errors here.

## Test harness (how to re-validate)

- `cargo build --release` builds the GUI (`tone`) + analyzer (`analyze`).
- `target/release/analyze <file.wav> [gate] [--timeline]` prints a pitch-track
  artifact report (median-filtered, matches the GUI).
- Python scripts (need numpy/scipy/matplotlib): `sweep.py` grades all test files,
  `eval.py` does per-segment scoring, `plot_final.py` renders the trace,
  `ground_truth.py` / `spectral_gt.py` are independent references.
- WAVs are decoded from the `.m4a` sources with ffmpeg (`-ac 1 -ar 48000
  -c:a pcm_f32le`); regenerated files are gitignored.

## Next steps (not started)

- Onset-attack wobble could be reduced with a dedicated onset detector or a
  shorter window during attack (adaptive window) — low priority, it's cosmetic.
- The three original goals still stand: targets + reference tone, fretboard
  sidebar, staff-notation sidebar.

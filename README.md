# tone

A small native desktop **pitch-detection toy**: captures the microphone in
real time, estimates the fundamental frequency, and plots a scrolling pitch
trace on a chromatic grid with note labels. Built to help with ear/voice
training, learning notes on the guitar neck, and reading notation.

Rust + [`egui`/`eframe`](https://github.com/emilk/egui) for the UI and
[`cpal`](https://github.com/RustAudio/cpal) for audio (CoreAudio on macOS).

## Run

```sh
cargo run --release
```

Top bar controls:

- **range** — vertical span of the grid (presets, or **pinch** to zoom)
- **center** — note the grid is centred on (presets, or **two-finger scroll** to pan)
- **gate** — RMS noise gate in dB; raise it to ignore quiet background, lower it
  to track faint decays. The live readout shows `Hz / note / cents`.
- **reference tone** — click anywhere on the plot to drop a target-note line
  (snapped to a semitone) and play a sine reference at that pitch; **drag** to
  retune, **click the same note** again to mute. Top bar has a play toggle and a
  volume slider. Use headphones so the tone doesn't leak into the mic — then sing
  against the line and watch your trace chase it.

macOS will prompt for microphone access on first launch.

## How pitch detection works

The detector (`src/lib.rs`) uses the **McLeod Pitch Method (MPM)**:

1. Compute the **Normalized Square Difference Function (NSDF)** over a 4096-sample
   window (~85 ms @ 48 kHz). NSDF is in `[-1, 1]`; its peaks mark periodicities
   and the peak height is a built-in clarity/confidence value.
2. **Octave correction** — take the strongest NSDF peak, then climb to a shorter
   integer sub-multiple only if that sub-period is *nearly as strong*
   (≥ `K_SUB`·gmax). A genuine sub-octave qualifies; a mere harmonic doesn't.
   This fixes octave-down errors on decaying strings without inducing octave-up
   on low notes — with no temporal state, so it never fights a real octave jump.
3. **Voicing** is gated by NSDF clarity + the RMS gate + an onset gate (a frame
   whose window still straddles silence is suppressed).
4. The drawn trace and readout are passed through a **5-frame median filter**,
   which removes 1–2-frame edge spikes while preserving vibrato and slides.

Detection range is ~60–1200 Hz. The detector is octave-stable from low notes
(~D2) up through the mid register, validated to zero octave errors on real
recordings and synthetic torture tones.

See `session_logs/2026-06-04_octave_error_nsdf_rewrite.md` for the design
rationale and the prior-art comparison with [FMIT](https://github.com/gillesdegottex/fmit).

## Offline analyzer & test harness

`src/bin/analyze.rs` runs the *exact same* detector over a WAV file and prints a
pitch-track artifact report (median pitch, octave-error counts, note histogram):

```sh
cargo build --release
target/release/analyze test_audio/e3_ref_48k_mono.wav 0.008 --timeline
```

`test_audio/` holds reference recordings (`*.m4a`) and Python validation scripts
(need `numpy`/`scipy`/`matplotlib`):

- `sweep.py` — grade the detector across all test files
- `eval.py` — per-segment scoring of one file
- `plot_final.py` — render the pitch track as an image
- `ground_truth.py`, `spectral_gt.py` — independent reference estimators

Test WAVs are decoded from the `.m4a` sources with ffmpeg and are gitignored:

```sh
ffmpeg -i test_audio/e3_ref.m4a -ac 1 -ar 48000 -c:a pcm_f32le test_audio/e3_ref_48k_mono.wav
```

## Status / roadmap

Working: live pitch trace, octave-stable detection, range/center/gate controls,
click/drag target line + sine reference tone.

Planned: a fretboard sidebar and a staff-notation sidebar (the same target note
shown on the neck and on a staff).

`index.html` is the original browser prototype, kept for reference.

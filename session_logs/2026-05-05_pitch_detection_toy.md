# 2026-05-05 — pitch detection toy: bootstrap

## What we built

Native rust desktop app (`cargo run --release`) that captures mic input, runs
pitch detection, and plots a scrolling pitch trace on a chromatic grid with
note labels on the right side. Defaults to 3 octaves centered on C3.

Stack: `cpal` (CoreAudio mic), `eframe`/`egui` (UI), custom YIN. Original
browser prototype (`index.html`) kept in repo for reference.

## Decisions

- **Browser → native rust.** Started in browser (Web Audio + canvas). Switched
  to native because `ScriptProcessorNode` events stalled mid-session and
  `AnalyserNode` polling didn't fully resolve it. WASM-in-browser wouldn't
  have helped — the flakiness is in Web Audio itself. Native cpal gives
  CoreAudio directly, no event-pump dependency.
- **Custom YIN, not a crate.** ~40 lines, gives full control over the
  thresholds. Crate alternatives (`pitch-detection`) hide the knobs.
- **No temporal smoothing on detection.** User explicitly wants raw
  measurement. All "smoothing" is either per-frame algorithmic correction
  (octave) or draw-time outlier rejection (spike islands). The underlying
  midi value stream is untouched.
- **Scale labels on the right edge.** Matches the indicator dot at the right
  of the scrolling trace; reading is left-to-right "what note am I on now".
- **Single root commit.** Toy stage; no need for granular history yet.

## Technical findings

- **The 2–3.5s "stops" mystery was the RMS gate.** Initial gate was 0.01
  (~−40dB), which chopped guitar plucks well before audibility and clipped
  sustained voice as breath weakened. Fixed by lowering default to 0.001
  (~−60dB) and exposing as a dB slider. Diagnosis came from user noting that
  guitar plucks (stable pitch, decaying amplitude) consistently cut sub-1s
  with a perfectly straight line — amplitude problem, not pitch problem.
- **Octave correction is a knife edge.** First attempt (accept multiple if
  cmnd ≤ 1.3× the candidate or below the absolute threshold) caused
  systematic octave-DOWN errors when climbing past ~C#3 — multiples of those
  τ values land just inside the search range (max τ corresponds to 70Hz,
  C2 ≈ 65Hz). Correct rule: only flip to a multiple if it's *meaningfully
  deeper* than the candidate (`cmnd[mult] < cmnd[t] * 0.8`). YIN's normalizer
  naturally makes a true fundamental's multiples slightly *shallower*, so a
  real octave-up error stands out as a multiple noticeably deeper.
- **Multi-frame YIN spikes need a wider rejection window.** Single-neighbor
  check (i−1, i+1) misses 2- and 3-frame outlier islands. Generalized to
  iterating window radii w ∈ {1,2,3}: a point is suppressed iff there exists
  some w where the points at i±w are close to each other and the current is
  far from both. Catches up to 3-frame islands; longer runs treated as real.

## Current status

Working: live pitch trace, octave-error correction, multi-frame spike
rejection at draw time, dB-scale RMS gate slider, range/center selectors.

UI is bare: top bar (range, center, gate, live readout) + plot. Defaults
land on C3 with 3-octave range.

## Next steps (discussed, not started)

User wants the toy to help with three goals: voice control, learning notes
on the guitar neck, learning traditional notation. Proposed unified path:

1. **Targets + anchor line + reference tone** — pick a note, see anchor on
   trace, hear it, sing it back. Real practice loop.
2. **Fretboard sidebar** — same target highlighted on guitar neck. Cross-
   modal binding (voice ↔ instrument) and the user's stated weak spot.
3. **Staff notation sidebar** — same target on treble/bass clef. Slowest
   payoff but unique to this tool.

User hasn't picked yet. May redirect to "fretboard only" or "notation first".

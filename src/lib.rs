//! Pitch-detection DSP, shared by the GUI binary and the offline analyzer.
//!
//! Monophonic f0 estimation via the McLeod Pitch Method (MPM): the Normalized
//! Square Difference Function (NSDF) plus clarity-thresholded peak picking.
//!
//! Why NSDF/MPM and not the autocorrelation/YIN it replaced: a plucked string
//! (or sung note) is harmonically rich, and as it decays the energy balance
//! shifts so that the *period-doubled* match (2·τ, one octave down) becomes as
//! good as — or better than — the true-period match. Plain "first dip below a
//! fixed threshold" YIN then jumps to the sub-octave, and an ad-hoc "flip to the
//! deeper multiple" correction makes it worse. MPM picks the FIRST (shortest-
//! period / highest-frequency) peak that reaches `K_PEAK` × the tallest peak,
//! which structurally refuses to drop an octave just because 2·τ scores a hair
//! higher. NSDF is normalized to [-1, 1], so the peak height doubles as a
//! clarity/confidence value and the voicing decision needs no magic energy
//! constant.

/// Analysis window: 4096 samples @48k ≈ 85 ms. Long enough to fit several
/// periods of the lowest notes (so their NSDF is clean and octave-stable), short
/// enough to keep onset latency and the silence→note window-fill smear modest.
pub const FRAME_SIZE: usize = 4096;

/// Lowest/highest detectable f0 (Hz). 60 Hz ≈ B1, 1200 Hz ≈ D6. The floor is
/// deliberately not lower: a wider lag range admits spurious peaks at high
/// harmonic multiples of mid/high notes that can outrank the fundamental.
const MIN_HZ: f32 = 60.0;
const MAX_HZ: f32 = 1200.0;

/// Peak-pick threshold: accept the first (shortest-period) NSDF peak reaching
/// this fraction of the tallest peak. Sits between a voice harmonic (~0.85·gmax)
/// and the fundamental (~0.95·gmax), so the scan skips overtones and lands on
/// the fundamental. Higher ⇒ more octave-UP risk; lower ⇒ more sub-octave risk.
const K_PEAK: f32 = 0.92;

/// Default voicing clarity floor (min tallest-peak NSDF for a frame to count as
/// pitched). Lower tracks quieter/breathier voice with fewer dropouts but
/// re-admits weak frames that can octave-slip on a decaying string; raise toward
/// ~0.85 for plucked-instrument cleanliness. Exposed as a UI slider.
pub const DEFAULT_CLARITY: f32 = 0.7;

/// Estimate the fundamental of `buf`. Returns `(hz, clarity, rms)`.
/// `hz == 0.0` (and `clarity == 0.0`) means unvoiced: below the RMS gate, or no
/// periodic structure reaching `clarity_floor`. `clarity` is the NSDF value at
/// the chosen peak, in [0, 1].
pub fn detect_pitch(buf: &[f32], sample_rate: u32, rms_gate: f32, clarity_floor: f32) -> (f32, f32, f32) {
    let n = buf.len();

    // RMS gate (cheap voicing / silence reject before the O(N·τ) inner loop)
    let mut sum_sq = 0.0f32;
    for &x in buf {
        sum_sq += x * x;
    }
    let rms = (sum_sq / n as f32).sqrt();
    if rms < rms_gate {
        return (0.0, 0.0, rms);
    }

    // Onset gate: also require the OLDEST quarter of the window to be above the
    // gate. While a note is filling the window (silence→note straddle just after
    // an attack) the pitch estimate is unreliable — it reads a transient partial.
    // Suppressing until the note fills the window costs ~one window of onset
    // latency but removes the wrong-pitch ramp at every note start.
    let head = n / 4;
    let mut head_sq = 0.0f32;
    for &x in &buf[..head] {
        head_sq += x * x;
    }
    if (head_sq / head as f32).sqrt() < rms_gate {
        return (0.0, 0.0, rms);
    }

    let sr = sample_rate as f32;
    let min_lag = (sr / MAX_HZ).floor() as usize;
    let max_lag = (sr / MIN_HZ).floor() as usize;
    if max_lag >= n || min_lag < 1 {
        return (0.0, 0.0, rms);
    }

    // NSDF (McLeod type-II): n'(τ) = 2·Σ x[i]·x[i+τ] / Σ (x[i]² + x[i+τ]²), where
    // the sum runs over the SHRINKING window i ∈ [0, n-τ). Using all available
    // samples per lag (not a fixed short window) is what makes low notes — whose
    // period is a large fraction of the frame — resolvable instead of octave-
    // jumping. Peaks at τ = period, 2·period, …; height = periodicity in [-1,1].
    let mut nsdf = vec![0.0f32; max_lag + 1];
    for tau in min_lag..=max_lag {
        let mut acf = 0.0f32; // numerator: cross-correlation at lag τ
        let mut m = 0.0f32; // denominator: summed energy of both windows
        for i in 0..(n - tau) {
            let a = buf[i];
            let b = buf[i + tau];
            acf += a * b;
            m += a * a + b * b;
        }
        nsdf[tau] = if m > 0.0 { 2.0 * acf / m } else { 0.0 };
    }

    // Collect local-maximum peaks and the tallest one's height (gmax).
    let mut peaks: Vec<usize> = Vec::new();
    let mut gmax = 0.0f32;
    for tau in (min_lag + 1)..max_lag {
        if nsdf[tau] > nsdf[tau - 1] && nsdf[tau] >= nsdf[tau + 1] && nsdf[tau] > 0.0 {
            peaks.push(tau);
            if nsdf[tau] > gmax {
                gmax = nsdf[tau];
            }
        }
    }
    if peaks.is_empty() || gmax < clarity_floor {
        return (0.0, 0.0, rms);
    }

    // McLeod selection: the FIRST (shortest-period / highest-frequency) peak that
    // reaches K_PEAK·gmax. Tuned for voice: scanning up from the shortest period
    // lands on the fundamental, because (a) a dominant SUB-octave peak sits at
    // 2·period — a longer lag, reached later — and (b) the voice's own harmonics
    // (H2/H3 at shorter lags) score below K_PEAK, so the scan skips them instead
    // of "jumping up" onto them. K_PEAK lives between the harmonic peak (~0.85)
    // and the fundamental (~0.95).
    let level = K_PEAK * gmax;
    let mut chosen = peaks[0];
    for &p in &peaks {
        if nsdf[p] >= level {
            chosen = p;
            break;
        }
    }

    // Parabolic interpolation on the NSDF around the chosen peak for sub-sample τ.
    let t = chosen;
    let t0 = t - 1;
    let t2 = (t + 1).min(max_lag);
    let (y0, y1, y2) = (nsdf[t0], nsdf[t], nsdf[t2]);
    let denom = y0 - 2.0 * y1 + y2;
    let tau_refined = if denom != 0.0 {
        t as f32 + 0.5 * (y0 - y2) / denom
    } else {
        t as f32
    };

    (sr / tau_refined, nsdf[t], rms)
}

pub fn hz_to_midi(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

pub fn note_name(midi: i32) -> String {
    let pc = ((midi % 12) + 12) % 12;
    let oct = midi.div_euclid(12) - 1;
    format!("{}{}", NOTE_NAMES[pc as usize], oct)
}

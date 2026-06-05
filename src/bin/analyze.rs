//! Offline pitch-track analyzer. Runs the GUI's exact detector over a WAV file
//! and reports artifact statistics, so DSP changes can be graded objectively
//! against a known recording instead of by eyeballing the live trace.
//!
//! usage: analyze <file.wav> [gate_rms] [--timeline]

use std::env;
use std::fs;
use tone::{detect_pitch, hz_to_midi, note_name, DEFAULT_CLARITY, FRAME_SIZE};

const UI_HZ: f32 = 60.0; // match the live app's sampling cadence

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: analyze <file.wav> [gate_rms] [--timeline]");
        std::process::exit(1);
    }
    let path = &args[1];
    let gate: f32 = args
        .iter()
        .skip(2)
        .find_map(|a| a.parse::<f32>().ok())
        .unwrap_or(0.001);
    let timeline = args.iter().any(|a| a == "--timeline");

    let (samples, sr) = read_wav(path).expect("failed to read wav");
    eprintln!(
        "{}: sr={} n={} dur={:.3}s gate={}",
        path,
        sr,
        samples.len(),
        samples.len() as f32 / sr as f32,
        gate
    );

    let csv = env::var("CSV").is_ok();
    let window: usize = env::var("WINDOW").ok().and_then(|v| v.parse().ok()).unwrap_or(FRAME_SIZE);
    let clarity: f32 = env::var("CLAR").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_CLARITY);

    let hop = (sr as f32 / UI_HZ).round() as usize; // ~800 @ 48k
    let mut track: Vec<(f32, f32, f32, f32)> = Vec::new(); // (t, hz, conf, rms)
    let mut s = 0usize;
    while s + window <= samples.len() {
        let (hz, conf, rms) = detect_pitch(&samples[s..s + window], sr, gate, clarity);
        track.push((s as f32 / sr as f32, hz, conf, rms));
        s += hop;
    }

    // raw per-frame midi (None = unvoiced), then the SAME 5-wide centered median
    // the GUI applies (median5): require >=3 voiced frames in the window, else
    // drop as an isolated blip. All downstream stats/CSV use this so the analyzer
    // reflects exactly what the app draws.
    let raw: Vec<Option<f32>> = track
        .iter()
        .map(|(_, hz, _, _)| if *hz > 0.0 { Some(hz_to_midi(*hz)) } else { None })
        .collect();
    let fmidi: Vec<Option<f32>> = (0..raw.len())
        .map(|i| {
            let lo = i.saturating_sub(2);
            let hi = (i + 2).min(raw.len() - 1);
            let mut v: Vec<f32> = (lo..=hi).filter_map(|j| raw[j]).collect();
            if v.len() < 3 {
                None
            } else {
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                Some(v[v.len() / 2])
            }
        })
        .collect();

    if csv {
        // t,midi,conf,rms  (midi empty when unvoiced) -> stdout for plotting
        println!("t,midi,conf,rms");
        for (i, (t, _, conf, rms)) in track.iter().enumerate() {
            match fmidi[i] {
                Some(m) => println!("{:.4},{:.4},{:.3},{:.5}", t, m, conf, rms),
                None => println!("{:.4},,{:.3},{:.5}", t, conf, rms),
            }
        }
        return;
    }

    let voiced: Vec<(f32, f32)> = track
        .iter()
        .enumerate()
        .filter_map(|(i, (t, _, _, _))| fmidi[i].map(|m| (*t, m)))
        .collect();

    if voiced.is_empty() {
        println!("no voiced frames");
        return;
    }

    let mut midis: Vec<f32> = voiced.iter().map(|(_, m)| *m).collect();
    midis.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = midis[midis.len() / 2];
    let nearest = median.round() as i32;

    // artifact accounting relative to the voiced median (the held note)
    let mut off_semi = 0; // > 0.5 semitone off
    let mut octave_low = 0; // ~12 below
    let mut octave_high = 0; // ~12 above
    let mut max_dev = 0.0f32;
    for (_, m) in &voiced {
        let dev = m - median;
        if dev.abs() > 0.5 {
            off_semi += 1;
        }
        if (dev + 12.0).abs() < 1.5 {
            octave_low += 1;
        }
        if (dev - 12.0).abs() < 1.5 {
            octave_high += 1;
        }
        max_dev = max_dev.max(dev.abs());
    }

    println!("\n=== {} ===", path);
    println!(
        "voiced frames: {}/{} ({:.0}%)",
        voiced.len(),
        track.len(),
        100.0 * voiced.len() as f32 / track.len() as f32
    );
    println!(
        "median pitch:  midi {:.2}  {}  ({:.2} Hz)",
        median,
        note_name(nearest),
        440.0 * 2f32.powf((median - 69.0) / 12.0)
    );
    println!(
        "ARTIFACTS:     {} frames >0.5 semitone off ({:.1}%)   max dev {:.1} semitones",
        off_semi,
        100.0 * off_semi as f32 / voiced.len() as f32,
        max_dev
    );
    println!(
        "  octave-down jumps: {}   octave-up jumps: {}",
        octave_low, octave_high
    );

    // note histogram of voiced frames
    use std::collections::BTreeMap;
    let mut hist: BTreeMap<i32, usize> = BTreeMap::new();
    for (_, m) in &voiced {
        *hist.entry(m.round() as i32).or_insert(0) += 1;
    }
    println!("voiced note histogram:");
    let mut items: Vec<_> = hist.into_iter().collect();
    items.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    for (midi, c) in items.iter().take(8) {
        println!(
            "  {:<4} {:>4} ({:.0}%)",
            note_name(*midi),
            c,
            100.0 * *c as f32 / voiced.len() as f32
        );
    }

    if env::var("FAULTS").is_ok() {
        eprintln!("\nvoiced frames >0.5 semitone off median (midi {:.2}):", median);
        for (i, (t, _, conf, rms)) in track.iter().enumerate() {
            if let Some(m) = fmidi[i] {
                let dev = m - median;
                if dev.abs() > 0.5 {
                    eprintln!(
                        "  {:6.2}s  {:<4}  dev={:+.2}st  conf={:.2}  rms={:.4}",
                        t,
                        note_name(m.round() as i32),
                        dev,
                        conf,
                        rms
                    );
                }
            }
        }
    }

    if timeline {
        println!("\ntimeline (t: note  hz  conf  rms):");
        let step = (track.len() / 60).max(1);
        for (i, (t, _, conf, rms)) in track.iter().enumerate() {
            if i % step != 0 {
                continue;
            }
            let lbl = if let Some(m) = fmidi[i] {
                let hz = 440.0 * 2f32.powf((m - 69.0) / 12.0);
                format!("{:<4} {:6.1}Hz c={:.2}", note_name(m.round() as i32), hz, conf)
            } else if *rms < gate {
                "silence".to_string()
            } else {
                format!("unvoiced (c={:.2})", conf)
            };
            println!("  {:5.2}s  {}  rms={:.4}", t, lbl, rms);
        }
    }
}

/// Minimal WAV reader: handles PCM int16 and IEEE float32, any channel count
/// (downmixed to mono). Enough for ffmpeg-produced test files.
fn read_wav(path: &str) -> Result<(Vec<f32>, u32), String> {
    let b = fs::read(path).map_err(|e| e.to_string())?;
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut pos = 12;
    let mut fmt_tag = 0u16;
    let mut channels = 1u16;
    let mut sample_rate = 0u32;
    let mut bits = 0u16;
    let mut samples: Vec<f32> = Vec::new();
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let sz = u32::from_le_bytes([b[pos + 4], b[pos + 5], b[pos + 6], b[pos + 7]]) as usize;
        let body = pos + 8;
        if id == b"fmt " {
            fmt_tag = u16::from_le_bytes([b[body], b[body + 1]]);
            channels = u16::from_le_bytes([b[body + 2], b[body + 3]]);
            sample_rate = u32::from_le_bytes([b[body + 4], b[body + 5], b[body + 6], b[body + 7]]);
            bits = u16::from_le_bytes([b[body + 14], b[body + 15]]);
        } else if id == b"data" {
            let end = (body + sz).min(b.len());
            let raw = &b[body..end];
            let ch = channels.max(1) as usize;
            match (fmt_tag, bits) {
                (3, 32) => {
                    let frames = raw.len() / 4;
                    for f in 0..(frames / ch) {
                        let mut acc = 0.0f32;
                        for c in 0..ch {
                            let o = (f * ch + c) * 4;
                            acc += f32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]);
                        }
                        samples.push(acc / ch as f32);
                    }
                }
                (1, 16) => {
                    let frames = raw.len() / 2;
                    for f in 0..(frames / ch) {
                        let mut acc = 0.0f32;
                        for c in 0..ch {
                            let o = (f * ch + c) * 2;
                            let v = i16::from_le_bytes([raw[o], raw[o + 1]]);
                            acc += v as f32 / 32768.0;
                        }
                        samples.push(acc / ch as f32);
                    }
                }
                other => return Err(format!("unsupported wav format tag/bits: {other:?}")),
            }
        }
        pos = body + sz + (sz & 1); // chunks are word-aligned
    }
    if sample_rate == 0 {
        return Err("no fmt chunk".into());
    }
    Ok((samples, sample_rate))
}

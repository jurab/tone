use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, Vec2};
use std::sync::{Arc, Mutex};

const FRAME_SIZE: usize = 2048;
const HIST_LEN: usize = 800;
const UI_HZ: f32 = 60.0;

// shared state between audio callback and ui thread
struct Shared {
    // most recent FRAME_SIZE samples (mono, f32, [-1,1])
    ring: Vec<f32>,
    write: usize,
}

impl Shared {
    fn new() -> Self {
        Self {
            ring: vec![0.0; FRAME_SIZE],
            write: 0,
        }
    }

    fn push(&mut self, samples: &[f32], channels: usize) {
        // downmix to mono by averaging channels
        let mut i = 0;
        while i + channels <= samples.len() {
            let mut s = 0.0f32;
            for c in 0..channels {
                s += samples[i + c];
            }
            s /= channels as f32;
            self.ring[self.write] = s;
            self.write = (self.write + 1) % FRAME_SIZE;
            i += channels;
        }
    }

    // copy the ring contents into `out` in chronological order (oldest first)
    fn snapshot(&self, out: &mut [f32]) {
        let n = out.len().min(FRAME_SIZE);
        let start = (self.write + FRAME_SIZE - n) % FRAME_SIZE;
        for i in 0..n {
            out[i] = self.ring[(start + i) % FRAME_SIZE];
        }
    }
}

// YIN-style pitch detection. returns (hz, confidence, rms).
// hz=0, conf=0 means unvoiced.
fn detect_pitch(buf: &[f32], sample_rate: u32, rms_gate: f32) -> (f32, f32, f32) {
    let n = buf.len();

    // RMS gate
    let mut sum_sq = 0.0f32;
    for &x in buf {
        sum_sq += x * x;
    }
    let rms = (sum_sq / n as f32).sqrt();
    if rms < rms_gate {
        return (0.0, 0.0, rms);
    }

    let sr = sample_rate as f32;
    let min_lag = (sr / 1200.0).floor() as usize;
    let max_lag = (sr / 70.0).floor() as usize;
    if max_lag >= n {
        return (0.0, 0.0, rms);
    }

    // difference function d[tau]
    let mut d = vec![0.0f32; max_lag + 1];
    for tau in min_lag..=max_lag {
        let mut s = 0.0f32;
        let lim = n - max_lag;
        for i in 0..lim {
            let diff = buf[i] - buf[i + tau];
            s += diff * diff;
        }
        d[tau] = s;
    }

    // cumulative mean normalized difference
    let mut cmnd = vec![1.0f32; max_lag + 1];
    let mut running = 0.0f32;
    for tau in 1..=max_lag {
        running += d[tau];
        if tau >= min_lag && running > 0.0 {
            cmnd[tau] = d[tau] * tau as f32 / running;
        }
    }

    // absolute threshold: first dip below 0.15
    let thresh = 0.15f32;
    let mut tau_est: i32 = -1;
    let mut tau = min_lag;
    while tau <= max_lag {
        if cmnd[tau] < thresh {
            // descend to local min
            while tau + 1 <= max_lag && cmnd[tau + 1] < cmnd[tau] {
                tau += 1;
            }
            tau_est = tau as i32;
            break;
        }
        tau += 1;
    }
    if tau_est < 0 {
        // fallback: global min, only accept if reasonable
        let mut best = min_lag;
        let mut bv = cmnd[min_lag];
        for t in (min_lag + 1)..=max_lag {
            if cmnd[t] < bv {
                bv = cmnd[t];
                best = t;
            }
        }
        if bv > 0.5 {
            return (0.0, 0.0, rms);
        }
        tau_est = best as i32;
    }

    // octave correction: only flip to a multiple if its dip is meaningfully
    // *deeper* than the candidate's. YIN's normalization makes a true
    // fundamental's multiples slightly shallower, so a real octave-up error
    // shows up as a multiple noticeably deeper than the candidate.
    let t0_est = tau_est as usize;
    let mut t = t0_est;
    for mult in 2..=4 {
        let center = t * mult;
        if center > max_lag {
            break;
        }
        let win = (t / 6).max(2);
        let lo = center.saturating_sub(win).max(min_lag);
        let hi = (center + win).min(max_lag);
        let (mut bi, mut bv) = (lo, cmnd[lo]);
        for k in (lo + 1)..=hi {
            if cmnd[k] < bv {
                bv = cmnd[k];
                bi = k;
            }
        }
        // require the multiple to be at least ~25% deeper than current candidate
        if bv < cmnd[t] * 0.8 {
            t = bi;
        } else {
            break;
        }
    }

    // parabolic interpolation around final t
    let t0 = if t > min_lag { t - 1 } else { t };
    let t2 = if t < max_lag { t + 1 } else { t };
    let y0 = cmnd[t0];
    let y1 = cmnd[t];
    let y2 = cmnd[t2];
    let denom = y0 - 2.0 * y1 + y2;
    let tau_refined = if denom != 0.0 {
        t as f32 + 0.5 * (y0 - y2) / denom
    } else {
        t as f32
    };

    let hz = sr / tau_refined;
    let confidence = 1.0 - cmnd[t];
    (hz, confidence, rms)
}

fn hz_to_midi(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

fn note_name(midi: i32) -> String {
    let pc = ((midi % 12) + 12) % 12;
    let oct = midi.div_euclid(12) - 1;
    format!("{}{}", NOTE_NAMES[pc as usize], oct)
}

struct App {
    shared: Arc<Mutex<Shared>>,
    _stream: Option<cpal::Stream>,
    sample_rate: u32,
    history: Vec<Option<f32>>,        // midi values (None = unvoiced)
    write_idx: usize,
    last_hz: f32,
    last_sample_t: f64,
    range_octaves: i32,
    center_midi: i32,
    rms_gate: f32,
    err: Option<String>,
}

impl App {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self {
            shared: Arc::new(Mutex::new(Shared::new())),
            _stream: None,
            sample_rate: 48000,
            history: vec![None; HIST_LEN],
            write_idx: 0,
            last_hz: 0.0,
            last_sample_t: 0.0,
            range_octaves: 3,
            center_midi: 48,
            rms_gate: 0.001,
            err: None,
        };
        if let Err(e) = app.start_audio() {
            app.err = Some(format!("audio init failed: {e}"));
        }
        app
    }

    fn start_audio(&mut self) -> Result<(), String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no default input device".to_string())?;
        let config = device
            .default_input_config()
            .map_err(|e| format!("{e}"))?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        self.sample_rate = sample_rate;
        *self.shared.lock().unwrap() = Shared::new();

        let shared = Arc::clone(&self.shared);
        let err_fn = |e| eprintln!("audio stream error: {e}");

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |data: &[f32], _| {
                    if let Ok(mut s) = shared.lock() {
                        s.push(data, channels);
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => {
                let shared = Arc::clone(&self.shared);
                device.build_input_stream(
                    &config.into(),
                    move |data: &[i16], _| {
                        let v: Vec<f32> = data.iter().map(|&x| x as f32 / 32768.0).collect();
                        if let Ok(mut s) = shared.lock() {
                            s.push(&v, channels);
                        }
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let shared = Arc::clone(&self.shared);
                device.build_input_stream(
                    &config.into(),
                    move |data: &[u16], _| {
                        let v: Vec<f32> = data
                            .iter()
                            .map(|&x| (x as f32 - 32768.0) / 32768.0)
                            .collect();
                        if let Ok(mut s) = shared.lock() {
                            s.push(&v, channels);
                        }
                    },
                    err_fn,
                    None,
                )
            }
            other => return Err(format!("unsupported sample format: {other:?}")),
        }
        .map_err(|e| format!("{e}"))?;

        stream.play().map_err(|e| format!("{e}"))?;
        self._stream = Some(stream);
        Ok(())
    }

    fn sample_pitch(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if now - self.last_sample_t < 1.0 / UI_HZ as f64 {
            return;
        }
        self.last_sample_t = now;

        let mut buf = vec![0.0f32; FRAME_SIZE];
        {
            let s = self.shared.lock().unwrap();
            s.snapshot(&mut buf);
        }

        let (hz, conf, _rms) = detect_pitch(&buf, self.sample_rate, self.rms_gate);
        self.last_hz = hz;
        self.history[self.write_idx] = if hz > 0.0 && conf > 0.5 {
            Some(hz_to_midi(hz))
        } else {
            None
        };
        self.write_idx = (self.write_idx + 1) % HIST_LEN;
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
        self.sample_pitch(ctx);

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(err) = &self.err {
                    ui.colored_label(Color32::from_rgb(220, 80, 80), err);
                    return;
                }
                ui.label("range:");
                egui::ComboBox::from_id_source("range")
                    .selected_text(format!("{} oct", self.range_octaves))
                    .show_ui(ui, |ui| {
                        for v in [2, 3, 4, 5] {
                            ui.selectable_value(&mut self.range_octaves, v, format!("{v} oct"));
                        }
                    });
                ui.label("center:");
                egui::ComboBox::from_id_source("center")
                    .selected_text(note_name(self.center_midi))
                    .show_ui(ui, |ui| {
                        for v in [48, 55, 60, 67, 72] {
                            ui.selectable_value(&mut self.center_midi, v, note_name(v));
                        }
                    });
                ui.label("gate:");
                // log slider for rms gate; dB display
                let mut gate_db = 20.0 * self.rms_gate.max(1e-6).log10();
                if ui
                    .add(egui::Slider::new(&mut gate_db, -90.0..=-20.0).suffix(" dB"))
                    .changed()
                {
                    self.rms_gate = 10f32.powf(gate_db / 20.0);
                }
                ui.separator();
                let cur = self.history[(self.write_idx + HIST_LEN - 1) % HIST_LEN];
                if let Some(m) = cur {
                    let nearest = m.round() as i32;
                    let cents = ((m - nearest as f32) * 100.0).round() as i32;
                    ui.label(format!(
                        "{:.1} Hz   {}   {:+}¢",
                        self.last_hz,
                        note_name(nearest),
                        cents
                    ));
                } else {
                    ui.label("— Hz   —   —¢");
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
            self.draw_plot(ui, rect);
        });
    }
}

impl App {
    fn draw_plot(&self, ui: &mut egui::Ui, rect: Rect) {
        let painter = ui.painter_at(rect);
        let bg = Color32::from_rgb(11, 11, 12);
        painter.rect_filled(rect, 0.0, bg);

        let scale_w = 50.0;
        let plot_right = rect.right() - scale_w;
        let plot_rect = Rect::from_min_max(
            Pos2::new(rect.left(), rect.top()),
            Pos2::new(plot_right, rect.bottom()),
        );

        let low_midi = self.center_midi - self.range_octaves * 6;
        let high_midi = self.center_midi + self.range_octaves * 6;
        let span = (high_midi - low_midi) as f32;
        let h = plot_rect.height();
        let midi_to_y = |m: f32| plot_rect.bottom() - ((m - low_midi as f32) / span) * h;

        // grid + labels
        for m in low_midi..=high_midi {
            let y = midi_to_y(m as f32);
            let pc = ((m % 12) + 12) % 12;
            let (col, label_col, font_sz) = if m == 69 {
                (
                    Color32::from_rgb(58, 74, 58),
                    Color32::from_rgb(122, 170, 136),
                    11.0,
                )
            } else if pc == 0 {
                (
                    Color32::from_rgb(42, 42, 48),
                    Color32::from_rgb(102, 102, 102),
                    11.0,
                )
            } else {
                (
                    Color32::from_rgb(21, 21, 26),
                    Color32::from_rgb(51, 51, 51),
                    10.0,
                )
            };
            painter.line_segment(
                [Pos2::new(plot_rect.left(), y), Pos2::new(plot_rect.right(), y)],
                Stroke::new(1.0, col),
            );
            painter.text(
                Pos2::new(plot_right + 8.0, y),
                egui::Align2::LEFT_CENTER,
                note_name(m),
                egui::FontId::monospace(font_sz),
                label_col,
            );
        }

        // separator
        painter.line_segment(
            [
                Pos2::new(plot_right, rect.top()),
                Pos2::new(plot_right, rect.bottom()),
            ],
            Stroke::new(1.0, Color32::from_rgb(34, 34, 34)),
        );

        // history trace with outlier-island rejection at draw time.
        // a point is suppressed iff there exists a window radius w ∈ {1,2,3}
        // such that the points at i±w bracket a stable signal (close to each
        // other) and the current point is far from both. this catches isolated
        // single-frame spikes as well as 2- and 3-frame islands; longer runs
        // are treated as real signal.
        let trace = Color32::from_rgb(136, 238, 255);
        let plot_w = plot_rect.width();
        let mut prev_pt: Option<Pos2> = None;
        const SPIKE_DELTA: f32 = 4.0;     // semitones from each shoulder
        const NEIGHBOR_DELTA: f32 = 2.0;  // semitones between the two shoulders
        const MAX_W: i32 = 3;

        let at = |i: i32| -> Option<f32> {
            if i < 0 || i >= HIST_LEN as i32 {
                None
            } else {
                self.history[(self.write_idx + i as usize) % HIST_LEN]
            }
        };

        for i in 0..HIST_LEN as i32 {
            let curr = at(i);
            let is_spike = match curr {
                Some(c) => (1..=MAX_W).any(|w| {
                    matches!((at(i - w), at(i + w)), (Some(l), Some(r))
                        if (l - r).abs() < NEIGHBOR_DELTA
                        && (c - l).abs() > SPIKE_DELTA
                        && (c - r).abs() > SPIKE_DELTA)
                }),
                None => false,
            };
            match curr {
                Some(m) if !is_spike => {
                    let x = plot_rect.left() + (i as f32 / (HIST_LEN as f32 - 1.0)) * plot_w;
                    let p = Pos2::new(x, midi_to_y(m));
                    if let Some(pp) = prev_pt {
                        painter.line_segment([pp, p], Stroke::new(2.0, trace));
                    }
                    prev_pt = Some(p);
                }
                Some(_) => {
                    // suppressed spike: keep prev_pt so the line bridges across
                }
                None => prev_pt = None,
            }
        }

        // current dot
        let cur = self.history[(self.write_idx + HIST_LEN - 1) % HIST_LEN];
        if let Some(m) = cur {
            let y = midi_to_y(m);
            painter.circle_filled(Pos2::new(plot_right, y), 4.0, trace);
        }
    }
}

fn main() -> Result<(), eframe::Error> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(Vec2::new(1100.0, 600.0))
            .with_title("tone"),
        ..Default::default()
    };
    eframe::run_native("tone", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, Vec2};
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tone::{detect_pitch, hz_to_midi, note_name, FRAME_SIZE};

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

/// Reference-tone oscillator state, shared lock-free with the audio output
/// callback. freq/gain are stored as f32 bits in atomics so the realtime thread
/// never blocks on a lock.
struct Tone {
    freq: AtomicU32,
    gain: AtomicU32, // target linear amplitude; 0 = silent
}

impl Tone {
    fn new() -> Self {
        Self {
            freq: AtomicU32::new(440f32.to_bits()),
            gain: AtomicU32::new(0.0f32.to_bits()),
        }
    }
    fn set_freq(&self, f: f32) {
        self.freq.store(f.to_bits(), Ordering::Relaxed);
    }
    fn freq(&self) -> f32 {
        f32::from_bits(self.freq.load(Ordering::Relaxed))
    }
    fn set_gain(&self, g: f32) {
        self.gain.store(g.to_bits(), Ordering::Relaxed);
    }
    fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }
}

struct App {
    shared: Arc<Mutex<Shared>>,
    _stream: Option<cpal::Stream>,
    tone: Arc<Tone>,
    _out_stream: Option<cpal::Stream>,
    target_midi: Option<f32>, // reference-tone target note (snapped to a semitone)
    tone_on: bool,
    tone_vol: f32,
    sample_rate: u32,
    history: Vec<Option<f32>>,        // midi values (None = unvoiced)
    write_idx: usize,
    last_sample_t: f64,
    range_octaves: f32, // octaves of vertical span (continuous, for pinch-zoom)
    center_midi: f32,   // grid center note (continuous, for scroll-pan)
    rms_gate: f32,
    err: Option<String>,
    needs_focus: bool,
}

impl App {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self {
            shared: Arc::new(Mutex::new(Shared::new())),
            _stream: None,
            tone: Arc::new(Tone::new()),
            _out_stream: None,
            target_midi: None,
            tone_on: false,
            tone_vol: 0.6,
            sample_rate: 48000,
            history: vec![None; HIST_LEN],
            write_idx: 0,
            last_sample_t: 0.0,
            range_octaves: 3.0,
            center_midi: 48.0,
            rms_gate: 0.0001, // -80 dB: very open; the NSDF clarity floor does the
            // real pitched/unpitched call, so the gate just skips dead silence.
            err: None,
            needs_focus: true,
        };
        if let Err(e) = app.start_audio() {
            app.err = Some(format!("audio init failed: {e}"));
        }
        if let Err(e) = app.start_output() {
            // non-fatal: the tuner still works without the reference tone
            eprintln!("reference-tone output unavailable: {e}");
        }
        app
    }

    fn start_output(&mut self) -> Result<(), String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default output device".to_string())?;
        let config = device.default_output_config().map_err(|e| format!("{e}"))?;
        let sr = config.sample_rate().0 as f32;
        let channels = config.channels() as usize;
        let tone = Arc::clone(&self.tone);
        let err_fn = |e| eprintln!("output stream error: {e}");

        // oscillator state lives in the callback; phase continuity + a one-pole
        // gain ramp toward the target avoid clicks on start/stop/retune.
        let mut phase = 0.0f32;
        let mut cur_gain = 0.0f32;

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config.into(),
                move |data: &mut [f32], _| {
                    let step = TAU * tone.freq() / sr;
                    let target = tone.gain();
                    for frame in data.chunks_mut(channels) {
                        cur_gain += (target - cur_gain) * 0.0008; // ~25 ms fade
                        let s = phase.sin() * cur_gain;
                        phase += step;
                        if phase >= TAU {
                            phase -= TAU;
                        }
                        for x in frame.iter_mut() {
                            *x = s;
                        }
                    }
                },
                err_fn,
                None,
            ),
            other => return Err(format!("unsupported output sample format: {other:?}")),
        }
        .map_err(|e| format!("{e}"))?;

        stream.play().map_err(|e| format!("{e}"))?;
        self._out_stream = Some(stream);
        Ok(())
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

        // the detector owns the voicing decision (RMS gate + NSDF clarity floor):
        // hz == 0 means unvoiced. trust it — no extra confidence threshold here.
        let (hz, _conf, _rms) = detect_pitch(&buf, self.sample_rate, self.rms_gate);
        self.history[self.write_idx] = if hz > 0.0 {
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
        if self.needs_focus {
            self.needs_focus = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        self.sample_pitch(ctx);

        // keep the audio output's oscillator in sync with the target + controls
        if let Some(m) = self.target_midi {
            self.tone.set_freq(440.0 * 2f32.powf((m - 69.0) / 12.0));
        }
        self.tone.set_gain(if self.tone_on && self.target_midi.is_some() {
            self.tone_vol
        } else {
            0.0
        });

        // view navigation: scroll to pan the center note, pinch (or ctrl+scroll)
        // to zoom the octave range. (flip the scroll sign if it feels inverted.)
        let (scroll_y, zoom) = ctx.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        if scroll_y != 0.0 {
            self.center_midi = (self.center_midi + scroll_y * 0.02).clamp(30.0, 90.0);
        }
        if zoom != 1.0 {
            self.range_octaves = (self.range_octaves / zoom).clamp(1.0, 6.0);
        }

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(err) = &self.err {
                    ui.colored_label(Color32::from_rgb(220, 80, 80), err);
                    return;
                }
                ui.label("range:");
                egui::ComboBox::from_id_source("range")
                    .selected_text(format!("{:.1} oct", self.range_octaves))
                    .show_ui(ui, |ui| {
                        for v in [2.0, 3.0, 4.0, 5.0] {
                            ui.selectable_value(&mut self.range_octaves, v, format!("{v:.0} oct"));
                        }
                    });
                ui.label("center:");
                egui::ComboBox::from_id_source("center")
                    .selected_text(note_name(self.center_midi.round() as i32))
                    .show_ui(ui, |ui| {
                        for v in [48, 55, 60, 67, 72] {
                            ui.selectable_value(&mut self.center_midi, v as f32, note_name(v));
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
                // readout uses the same median-filtered value as the trace, so a
                // momentary glitch frame can't flicker the displayed note.
                if let Some(m) = self.median5(HIST_LEN - 1) {
                    let nearest = m.round() as i32;
                    let cents = ((m - nearest as f32) * 100.0).round() as i32;
                    let hz = 440.0 * 2f32.powf((m - 69.0) / 12.0);
                    ui.label(format!("{:.1} Hz   {}   {:+}¢", hz, note_name(nearest), cents));
                } else {
                    ui.label("— Hz   —   —¢");
                }

                // reference tone: click the plot to set the target note, then play it
                ui.separator();
                let label = match self.target_midi {
                    Some(m) => format!("♪ play {}", note_name(m.round() as i32)),
                    None => "♪ click plot to set".to_string(),
                };
                ui.add_enabled_ui(self.target_midi.is_some(), |ui| {
                    ui.toggle_value(&mut self.tone_on, label);
                });
                ui.label("vol:");
                ui.spacing_mut().slider_width = 90.0;
                ui.add(
                    egui::Slider::new(&mut self.tone_vol, 0.0..=1.0)
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                );
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
            // click anywhere on the plot to drop a target-note line (snapped to a
            // semitone) and start the reference tone there.
            // click a note to set + play it; click the same note again to mute;
            // drag to retune continuously (snapped to semitones).
            let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
            let plot_right = rect.right() - 50.0; // matches draw_plot scale_w
            let low = self.center_midi - self.range_octaves * 6.0;
            let high = self.center_midi + self.range_octaves * 6.0;
            let pos_to_note = |pos: Pos2| {
                let frac = (rect.bottom() - pos.y) / rect.height();
                (low + frac * (high - low)).round()
            };
            if resp.dragged() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    if pos.x <= plot_right {
                        self.target_midi = Some(pos_to_note(pos));
                        self.tone_on = true;
                    }
                }
            } else if resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    if pos.x <= plot_right {
                        let m = pos_to_note(pos);
                        if self.target_midi == Some(m) {
                            self.tone_on = !self.tone_on; // same note → toggle mute
                        } else {
                            self.target_midi = Some(m);
                            self.tone_on = true;
                        }
                    }
                }
            }
            self.draw_plot(ui, rect);
        });
    }
}

impl App {
    /// 5-wide centered median of the voiced pitch history around chronological
    /// index `i` (0 = oldest on screen). Kills the 1-2 frame octave/onset spikes
    /// the per-frame detector still emits at note edges, while leaving sustained
    /// pitch — and vibrato/slides slower than ~5 frames — untouched. Returns None
    /// when the window holds fewer than 3 voiced frames (an isolated blip, not a
    /// real note).
    fn median5(&self, i: usize) -> Option<f32> {
        let lo = i.saturating_sub(2);
        let hi = (i + 2).min(HIST_LEN - 1);
        let mut vals: Vec<f32> = (lo..=hi)
            .filter_map(|j| self.history[(self.write_idx + j) % HIST_LEN])
            .collect();
        if vals.len() < 3 {
            return None;
        }
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        Some(vals[vals.len() / 2])
    }

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

        let low_midi = self.center_midi - self.range_octaves * 6.0;
        let high_midi = self.center_midi + self.range_octaves * 6.0;
        let span = high_midi - low_midi;
        let h = plot_rect.height();
        let midi_to_y = |m: f32| plot_rect.bottom() - ((m - low_midi) / span) * h;

        // grid + labels — integer note lines within the (continuous) range
        for m in low_midi.ceil() as i32..=high_midi.floor() as i32 {
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

        // target reference line (set by clicking the plot)
        if let Some(m) = self.target_midi {
            let y = midi_to_y(m);
            let col = if self.tone_on {
                Color32::from_rgb(255, 196, 92) // amber, lit when sounding
            } else {
                Color32::from_rgb(150, 120, 70)
            };
            painter.line_segment(
                [Pos2::new(plot_rect.left(), y), Pos2::new(plot_rect.right(), y)],
                Stroke::new(if self.tone_on { 2.0 } else { 1.5 }, col),
            );
            painter.text(
                Pos2::new(plot_rect.left() + 6.0, y - 4.0),
                egui::Align2::LEFT_BOTTOM,
                format!("target {}", note_name(m.round() as i32)),
                egui::FontId::monospace(11.0),
                col,
            );
        }

        // history trace, median-filtered (see median5): connect consecutive
        // voiced frames, break on unvoiced.
        let trace = Color32::from_rgb(136, 238, 255);
        let plot_w = plot_rect.width();
        let mut prev_pt: Option<Pos2> = None;

        for i in 0..HIST_LEN {
            match self.median5(i) {
                Some(m) => {
                    let x = plot_rect.left() + (i as f32 / (HIST_LEN as f32 - 1.0)) * plot_w;
                    let p = Pos2::new(x, midi_to_y(m));
                    if let Some(pp) = prev_pt {
                        painter.line_segment([pp, p], Stroke::new(2.0, trace));
                    }
                    prev_pt = Some(p);
                }
                None => prev_pt = None,
            }
        }

        // current dot
        if let Some(m) = self.median5(HIST_LEN - 1) {
            let y = midi_to_y(m);
            painter.circle_filled(Pos2::new(plot_right, y), 4.0, trace);
        }
    }
}

fn main() -> Result<(), eframe::Error> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(Vec2::new(1100.0, 600.0))
            .with_active(true)
            .with_title("tone"),
        ..Default::default()
    };
    eframe::run_native("tone", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

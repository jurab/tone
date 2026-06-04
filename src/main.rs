use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, Vec2};
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

struct App {
    shared: Arc<Mutex<Shared>>,
    _stream: Option<cpal::Stream>,
    sample_rate: u32,
    history: Vec<Option<f32>>,        // midi values (None = unvoiced)
    write_idx: usize,
    last_sample_t: f64,
    range_octaves: i32,
    center_midi: i32,
    rms_gate: f32,
    err: Option<String>,
    needs_focus: bool,
}

impl App {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self {
            shared: Arc::new(Mutex::new(Shared::new())),
            _stream: None,
            sample_rate: 48000,
            history: vec![None; HIST_LEN],
            write_idx: 0,
            last_sample_t: 0.0,
            range_octaves: 3,
            center_midi: 48,
            rms_gate: 0.008, // ~-42 dB: rejects sub-note room/string noise while
            // keeping the resolvable part of a decaying pluck. NSDF clarity does
            // the pitched/unpitched call above this; raise via the slider for
            // very quiet sustained material (voice tails).
            err: None,
            needs_focus: true,
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
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
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

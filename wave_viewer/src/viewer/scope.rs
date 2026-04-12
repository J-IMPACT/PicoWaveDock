use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use super::WINDOW_SIZE;

use crate::params::ArcParams;
use crate::viewer::ViewerAxisY;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TriggerMode { Auto, Normal, Single }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TriggerEdge { Rising, Falling }

#[derive(Clone, Copy, Debug)]
struct Trigger {
    mode: TriggerMode,
    edge: TriggerEdge,
    level: u16,
    hysteresis: u16,
    pretrigger_ratio: f32,  // 0.0..1.0 (typically <= 0.9)
    holdoff_ms: u64         // 0..200 etc.
}

pub struct ScopeApp {
    rx: Receiver<Vec<u16>>,
    params: ArcParams,

    samples: VecDeque<u16>,

    frames: VecDeque<Vec<u16>>,

    persistence: usize,

    decimation_gui: isize,

    y_axis: ViewerAxisY,

    trig: Trigger,
    last_triggered_at: Option<Instant>,
    single_armed: bool,

    // UI helpers
    t_offset_s: f64,
}

impl ScopeApp {
    pub fn new(
        rx: Receiver<Vec<u16>>,
        params: ArcParams,
        y_axis: ViewerAxisY
    ) -> Self {
        let decimation_gui = params.param0.load(Ordering::Relaxed);

        Self {
            rx,
            params,
            samples: VecDeque::with_capacity(WINDOW_SIZE * 10),

            frames: VecDeque::new(),

            persistence: 3,

            decimation_gui,

            y_axis,

            trig: Trigger {
                mode: TriggerMode::Auto,
                edge: TriggerEdge::Rising,
                level: 2048,
                hysteresis: 8,
                pretrigger_ratio: 0.2,
                holdoff_ms: 0,
            },
            last_triggered_at: None,
            single_armed: true,

            t_offset_s: 0.0,
        }
    }

    fn effective_sample_rate_hz(&self) -> f64 {
        let d = self.params.param0.load(Ordering::Relaxed) as f64;
        let speed = self.params.speed.load(Ordering::Relaxed) as f64;
        speed / (d + 1.0)
    }

    fn pretrigger_samples(&self) -> usize {
        ((WINDOW_SIZE as f32) * self.trig.pretrigger_ratio.clamp(0.0, 0.95)) as usize
    }

    fn push_samples(&mut self, chunk: &[u16]) {
        let keep = (WINDOW_SIZE * 8).max(32_768);

        for &v in chunk {
            if self.samples.len() >= keep {
                self.samples.pop_front();
            }
            self.samples.push_back(v);
        }

        self.y_axis.update(&self.samples);
    }

    fn within_holdoff(&self) -> bool {
        if let (Some(last), holdoff) = (self.last_triggered_at, self.trig.holdoff_ms) {
            holdoff > 0 && last.elapsed() < Duration::from_millis(holdoff)
        } else {
            false
        }
    }

    fn find_trigger_index(&self) -> Option<usize> {
        let n = self.samples.len();

        if n < 2 || self.within_holdoff() {
            return None;
        }

        let level = self.trig.level as i32;
        let hys = self.trig.hysteresis as i32;

        // Define thresholds with hysteresis:
        // Rising: arm when below (level - hys), fire when above (level + hys)
        // Falling: arm when below (level + hys), fire when above (level - hys)
        let lo = level - hys;
        let hi = level + hys;

        // Scan window: last ~2*window samples for responsiveness (or whole buffer if smaller)
        let scan_len = (WINDOW_SIZE * 2).min(n - 1);
        let start = n - 1 - scan_len;

        match self.trig.edge {
            TriggerEdge::Rising => {
                let mut armed = false;
                for i in start..(n - 1) {
                    let a = self.samples[i] as i32;
                    let b = self.samples[i + 1] as i32;

                    if !armed {
                        if a <= lo { armed = true; }
                        continue;
                    }

                    if a <= hi && b >= hi { return Some(i + 1); }
                }
            }
            TriggerEdge::Falling => {
                let mut armed = false;
                for i in start..(n - 1) {
                    let a = self.samples[i] as i32;
                    let b = self.samples[i + 1] as i32;

                    if !armed {
                        if a >= hi { armed = true; }
                        continue;
                    }

                    if a >= lo && b <= lo { return Some(i + 1); }
                }
            }
        }

        // let start = n.saturating_sub()
        None
    }

    fn capture_frame_at(&mut self, trig_idx: usize) -> bool {
        let n = self.samples.len();
        if n < WINDOW_SIZE { return false; }

        let pre = self.pretrigger_samples();

        // Frame start so that trigger lands as index `pre` inside frame.
        let start = trig_idx as isize - pre as isize;
        if start < 0 { return false; }
        let start = start as usize;
        let end = start + WINDOW_SIZE;
        if end > n { return false; }

        let mut raw = Vec::with_capacity(WINDOW_SIZE);
        for i in start..end {
            raw.push(self.samples[i])
        }

        self.frames.push_back(raw);
        while self.frames.len() > self.persistence.max(1) {
            self.frames.pop_front();
        }

        self.last_triggered_at = Some(Instant::now());

        if self.trig.mode == TriggerMode::Single {
            self.single_armed = false;
            self.params.paused.store(true, Ordering::Relaxed);
        }

        true
    }

    fn capture_latest_window(&mut self) -> bool {
        let n = self.samples.len();
        if n < WINDOW_SIZE {
            return false;
        }
        let start = n - WINDOW_SIZE;

        let mut raw = Vec::with_capacity(WINDOW_SIZE);
        for i in start..n {
            raw.push(self.samples[i]);
        }

        self.frames.push_back(raw);
        while self.frames.len() > self.persistence.max(1) {
            self.frames.pop_front();
        }

        true
    }

    fn maybe_capture(&mut self) -> bool {
        if self.trig.mode == TriggerMode::Single && !self.single_armed {
            return false;
        }

        if let Some(ti) = self.find_trigger_index() {
            return self.capture_frame_at(ti);
        }

        if self.trig.mode == TriggerMode::Auto {
            return self.capture_latest_window();
        }

        false
    }

    /// Draw one frame as line segments directly (no Vec creation / no clone).
    /// Also does horizontal decimation: if there are more points than pixels,
    /// step over samples.
    fn draw_frame_segments(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        raw: &[u16],
        stroke: egui::Stroke,
    ) {
        if raw.len() < 2 {
            return;
        }

        // Horizontal decimation: 1 segment per ~pixel
        let w_px = rect.width().max(1.0) as usize;
        let n = raw.len();
        let step = ((n - 1) / w_px).max(1); // >=1

        let y0 = self.y_axis.y_min;
        let y1 = self.y_axis.y_max.max(y0 + 1.0);
        let inv = 1.0 / (y1 - y0);

        let dx = rect.width() / ( (n - 1) as f32 );

        // First point
        let mut i0 = 0usize;
        let mut x0 = rect.left();
        let mut y0p = {
            let yn = ((raw[i0] as f32) - y0) * inv;
            rect.bottom() - (yn.clamp(0.0, 1.0) * rect.height())
        };

        let mut i = step;
        while i < n {
            let x1 = rect.left() + (i as f32) * dx;
            let y1p = {
                let yn = ((raw[i] as f32) - y0) * inv;
                rect.bottom() - (yn.clamp(0.0, 1.0) * rect.height())
            };

            painter.line_segment([egui::pos2(x0, y0p), egui::pos2(x1, y1p)], stroke);

            x0 = x1;
            y0p = y1p;
            i0 = i;
            i += step;
        }

        // Ensure we end at last sample
        if i0 != n - 1 {
            let x1 = rect.right();
            let y1p = {
                let yn = ((raw[n - 1] as f32) - y0) * inv;
                rect.bottom() - (yn.clamp(0.0, 1.0) * rect.height())
            };
            painter.line_segment([egui::pos2(x0, y0p), egui::pos2(x1, y1p)], stroke);
        }
    }
}

impl eframe::App for ScopeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let is_paused = self.params.paused.load(Ordering::Relaxed);

        let mut got_data = false;
        if !is_paused {
            while let Ok(chunk) = self.rx.try_recv() {
                self.push_samples(&chunk);
                got_data = true;
            }
        } else {
            while let Ok(_chunk) = self.rx.try_recv() {}
        }

        let mut captured = false;
        if !is_paused {
            captured = self.maybe_capture();
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.y_axis.auto_scale, "auto scale");
                if !self.y_axis.auto_scale {
                    ui.add(egui::DragValue::new(&mut self.y_axis.y_min).speed(1.0).prefix("y_min "));
                    ui.add(egui::DragValue::new(&mut self.y_axis.y_max).speed(1.0).prefix("y_max "));
                }

                ui.separator();

                let label = if is_paused { "▶ Start" } else { "⏸ Stop" };
                if ui.button(label).clicked() {
                    self.params.paused.store(!is_paused, Ordering::Relaxed);
                }
                if ui.button("clear").clicked() {
                    self.samples.clear();
                    self.frames.clear();
                }

                ui.separator();

                ui.label("trigger:");
                egui::ComboBox::new("trig_mode", "mode")
                    .selected_text(format!("{:?}", self.trig.mode))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.trig.mode, TriggerMode::Auto, "Auto");
                        ui.selectable_value(&mut self.trig.mode, TriggerMode::Normal, "Normal");
                        ui.selectable_value(&mut self.trig.mode, TriggerMode::Single, "Single");
                    });
                egui::ComboBox::new("trig_edge", "mode")
                    .selected_text(format!("{:?}", self.trig.edge))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.trig.edge, TriggerEdge::Rising, "Rising");
                        ui.selectable_value(&mut self.trig.edge, TriggerEdge::Falling, "Falling");
                    });

                ui.add(egui::DragValue::new(&mut self.trig.level).speed(1.0).range(0..=4095).prefix("level "));
                ui.add(egui::DragValue::new(&mut self.trig.hysteresis).speed(1.0).range(0..=512).prefix("hys "));
                ui.add(
                    egui::DragValue::new(&mut self.trig.pretrigger_ratio)
                        .speed(0.01)
                        .range(0.0..=0.95)
                        .prefix("pre "),
                );
                ui.add(
                    egui::DragValue::new(&mut self.trig.holdoff_ms)
                        .speed(1.0)
                        .range(0..=500)
                        .prefix("holdoff(ms) "),
                );

                if self.trig.mode == TriggerMode::Single {
                    let armed_label = if self.single_armed { "armed" } else { "re-arm" };
                    if ui.button(armed_label).clicked() {
                        self.single_armed = true;
                        self.params.paused.store(false, Ordering::Relaxed);
                    }
                }

                ui.separator();

                ui.add(egui::DragValue::new(&mut self.persistence).speed(1.0).range(1..=20).prefix("persistence "));
            });
        });

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Fs_eff: {:.1} Hz", self.effective_sample_rate_hz()));
                ui.separator();

                let mut d = self.decimation_gui;
                let resp = ui.add(
                    egui::DragValue::new(&mut d)
                        .speed(1.0)
                        .range(0..=4095)
                        .clamp_existing_to_range(true)
                        .prefix("dec "),
                );
                if resp.changed() {
                    self.decimation_gui = d;
                    self.params.param0.value.store(d, Ordering::Relaxed);
                }

                ui.separator();
                ui.add(egui::DragValue::new(&mut self.t_offset_s).speed(0.001).prefix("t0 "));
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let avail = ui.available_size();
            let (rect, _resp) = ui.allocate_exact_size(avail, egui::Sense::hover());

            let painter = ui.painter();
            painter.rect_filled(rect, 0.0, egui::Color32::from_gray(10));

            // Trigger vertical line at pretrigger location
            let pre = self.pretrigger_samples().min(WINDOW_SIZE.saturating_sub(1));
            let x_trig = if WINDOW_SIZE > 1 {
                rect.left() + rect.width() * (pre as f32 / (WINDOW_SIZE as f32 - 1.0))
            } else {
                rect.left()
            };
            painter.line_segment(
                [egui::pos2(x_trig, rect.top()), egui::pos2(x_trig, rect.bottom())],
                egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
            );

            // Trigger level line
            let y0 = self.y_axis.y_min;
            let y1 = self.y_axis.y_max.max(y0 + 1.0);
            let yn = ((self.trig.level as f32 - y0) / (y1 - y0)).clamp(0.0, 1.0);
            let y_level = rect.bottom() - yn * rect.height();
            painter.line_segment(
                [egui::pos2(rect.left(), y_level), egui::pos2(rect.right(), y_level)],
                egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
            );

            if self.frames.is_empty() {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "waiting for frames...",
                    egui::FontId::proportional(16.0),
                    egui::Color32::from_gray(180),
                );
            } else {
                // Draw oldest->newest with simple persistence coloring
                for (i, raw) in self.frames.iter().enumerate() {
                    let newest = i + 1 == self.frames.len();
                    let stroke = if newest {
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(0, 220, 160))
                    } else {
                        egui::Stroke::new(1.0, egui::Color32::from_gray(120))
                    };
                    self.draw_frame_segments(painter, rect, raw, stroke);
                }
            }
        });

        if !is_paused {
            if got_data || captured {
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(Duration::from_millis(16));
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.params.stop.store(true, Ordering::Relaxed);
    }
}
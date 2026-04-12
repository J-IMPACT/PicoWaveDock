use egui_plot::{Line, Plot, PlotPoints};
use std::{
    sync::{
        atomic::Ordering,
        mpsc,
    },
    time::Duration,
};

use crate::dsp::fft::SpectrumMsg;
use crate::params::ArcParams;

/* ===========================
   GUI app
   =========================== */

pub struct SpectrumApp {
    rx: mpsc::Receiver<SpectrumMsg>,
    params: ArcParams,

    spec: Vec<(f32, f32)>,
    fs_out_hz: f64,

    fft_n_gui: isize,
    avg_frames_gui: isize,
    f_pass_gui: isize,
    f_stop_gui: isize,
    fir_taps_gui: isize,

    y_min: f32,
    y_max: f32,
    auto_scale: bool,
}

impl SpectrumApp {
    pub fn new(
        rx: mpsc::Receiver<SpectrumMsg>,
        params: ArcParams,
    ) -> Self {
        let fft_n_gui = params.param0.load(Ordering::Relaxed);
        let avg_frames_gui = params.param1.load(Ordering::Relaxed);
        let f_pass_gui = params.param2.load(Ordering::Relaxed);
        let f_stop_gui = params.param3.load(Ordering::Relaxed);
        let fir_taps_gui = params.param4.load(Ordering::Relaxed);

        Self {
            rx, 
            params,
            spec: Vec::new(),
            fs_out_hz: 0.0,
            fft_n_gui,
            avg_frames_gui,
            f_pass_gui,
            f_stop_gui,
            fir_taps_gui,

            y_min: -120.0,
            y_max: 0.0,
            auto_scale: true,
        }
    }
}

impl eframe::App for SpectrumApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let is_paused = self.params.paused.load(Ordering::Relaxed);

        if !is_paused {
            while let Ok(msg) = self.rx.try_recv() {
                self.spec = msg.points;
                self.fs_out_hz = msg.fs_out_hz;
            }
        } else {
            while let Ok(_msg) = self.rx.try_recv() {}
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let label = if is_paused { "▶ Start" } else { "⏸ Stop" };
                if ui.button(label).clicked() {
                    self.params.paused.store(!is_paused, Ordering::Relaxed);
                }

                ui.separator();
                ui.checkbox(&mut self.auto_scale, "auto scale");
                if !self.auto_scale {
                    ui.add(egui::DragValue::new(&mut self.y_min).speed(1.0).prefix("y_min "));
                    ui.add(egui::DragValue::new(&mut self.y_max).speed(1.0).prefix("y_max "));
                }

                ui.separator();
                ui.label(format!("fs_out = {:.0} Hz", self.fs_out_hz));
            });
        });

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("FFT_N");
                if ui.add(egui::DragValue::new(&mut self.fft_n_gui).speed(256.0).range(256..=32768)).changed() {
                    // 推奨：2の冪に寄せる
                    let v = (self.fft_n_gui as u32).next_power_of_two().clamp(256, 32768);
                    self.fft_n_gui = v as isize;
                    self.params.param0.value.store(self.fft_n_gui, Ordering::Relaxed);
                }

                ui.separator();
                ui.label("avg");
                if ui.add(egui::DragValue::new(&mut self.avg_frames_gui).speed(1.0).range(1..=64)).changed() {
                    self.params.param1.value.store(self.avg_frames_gui, Ordering::Relaxed);
                }

                ui.separator();
                ui.label("F_pass");
                if ui.add(egui::DragValue::new(&mut self.f_pass_gui).speed(1000.0).range(10_000..=300_000)).changed() {
                    self.params.param2.value.store(self.f_pass_gui, Ordering::Relaxed);
                }

                ui.separator();
                ui.label("F_stop");
                if ui.add(egui::DragValue::new(&mut self.f_stop_gui).speed(1000.0).range(20_000..=370_000)).changed() {
                    // ensure stop > pass
                    if self.f_stop_gui <= self.f_pass_gui + 1000 {
                        self.f_stop_gui = self.f_pass_gui + 1000;
                    }
                    self.params.param3.value.store(self.f_stop_gui, Ordering::Relaxed);
                }

                ui.separator();
                ui.label("FIR taps");
                if ui.add(egui::DragValue::new(&mut self.fir_taps_gui).speed(2.0).range(7..=255)).changed() {
                    // odd
                    self.fir_taps_gui |= 1;
                    self.params.param4.value.store(self.fir_taps_gui, Ordering::Relaxed);
                }

                ui.separator();
                let nyq = (self.fs_out_hz * 0.5) as i32;
                ui.label(format!("Nyq = {} Hz", nyq));

                // ui.separator();
                // ui.checkbox(&mut self.log_x, "log f");
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.spec.is_empty() {
                ui.label("waiting for spectrum...");
                return;
            }

            let mut pts: Vec<[f64; 2]> = Vec::with_capacity(self.spec.len());
            let mut mn = f32::INFINITY;
            let mut mx = f32::NEG_INFINITY;

            for &(f, db) in &self.spec {
                pts.push([f as f64, db as f64]);
                mn = mn.min(db);
                mx = mx.max(db);
            }

            if self.auto_scale {
                self.y_min = (mn - 6.0).floor();
                self.y_max = (mx + 6.0).ceil();
            }

            Plot::new("spectrum")
                .allow_drag(true)
                .allow_zoom(true)
                .allow_scroll(true)
                .include_y(self.y_min as f64)
                .include_y(self.y_max as f64)
                .show(ui, |plot_ui| {
                    plot_ui.line(Line::new("mag", PlotPoints::from(pts)));
                });
        });

        if !is_paused {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.params.stop.store(true, Ordering::Relaxed);
    }
}
use egui_plot::{Line, Plot, PlotPoints};

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use super::{ViewerAxisY, WINDOW_SIZE};

use crate::data::Data;
use crate::params::ArcParams;

pub struct PlotApp<T: Data> {
    rx: mpsc::Receiver<Vec<T>>,
    
    samples: VecDeque<T>,
    plot_work: Vec<[f64; 2]>,
    plot_send: Vec<[f64; 2]>,

    params: ArcParams,
    decimation_gui: isize,

    y_axis: ViewerAxisY
}

impl<T: Data> PlotApp<T> {
    pub fn new(
        rx: mpsc::Receiver<Vec<T>>, 
        params: ArcParams,
        y_axis: ViewerAxisY
    ) -> Self {
        let decimation_gui = params.param0.load(Ordering::Relaxed);
        Self {
            rx, samples: VecDeque::with_capacity(WINDOW_SIZE),
            plot_work: (0..WINDOW_SIZE).map(|i| [i as f64, 0.0]).collect::<Vec<[f64; 2]>>(),
            plot_send: vec![[0.0; 2]; WINDOW_SIZE],
            params, decimation_gui,
            y_axis
        }
    }
    fn trim(&mut self) {
        while self.samples.len() > WINDOW_SIZE {
            self.samples.pop_front();
        }
    }
    fn ensure_plot_buffers(&mut self) {
        if self.plot_work.len() != WINDOW_SIZE {
            self.plot_work = (0..WINDOW_SIZE).map(|i| [i as f64, 0.0]).collect::<Vec<[f64; 2]>>();
            self.plot_send = vec![[0.0; 2]; WINDOW_SIZE];
        }
    }
    fn push_samples(&mut self, chunk: &[T]) {
        self.trim();
        for &v in chunk {
            if self.samples.len() >= WINDOW_SIZE {
                self.samples.pop_front();
            }
            self.samples.push_back(v);
        }
        self.y_axis.update(&self.samples);
    }
}

impl<T: Data> eframe::App for PlotApp<T> {
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

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.y_axis.auto_scale, "auto scale");
                if !self.y_axis.auto_scale {
                    ui.add(egui::DragValue::new(&mut self.y_axis.y_min).speed(1.0).prefix("y_min "));
                    ui.add(egui::DragValue::new(&mut self.y_axis.y_max).speed(1.0).prefix("y_max "));
                }

                ui.separator();

                let is_paused = self.params.paused.load(Ordering::Relaxed);
                let label = if is_paused { "▶ Start" } else { "⏸ Stop" };

                if ui.button(label).clicked() {
                    self.params.paused.store(!is_paused, Ordering::Relaxed);
                }
                if ui.button("clear").clicked() {
                    self.samples.clear();
                }
            });
        });

        self.ensure_plot_buffers();

        for (wi, &v) in self.samples.iter().enumerate() {
            self.plot_work[wi][1] = v.to_f64().unwrap();
        }

        self.plot_send.clear();
        self.plot_send.extend_from_slice(&self.plot_work);

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.samples.len() == 0 {
                ui.label("waiting for data...");
            } else {
                Plot::new("adc_plot")
                    .allow_drag(false)
                    .allow_zoom(false)
                    .allow_scroll(false)
                    .show_x(false)
                    .show_y(false)
                    .include_y(self.y_axis.y_min as f64)
                    .include_y(self.y_axis.y_max as f64)
                    .show(ui, |plot_ui| {
                        let points_vec = std::mem::take(&mut self.plot_send);
                        let line = Line::new(format!("line"), PlotPoints::from(points_vec));
                        plot_ui.line(line);
                    });
            }
        });

        let capacity = self.plot_send.capacity();
        if capacity < WINDOW_SIZE {
            self.plot_send.reserve(WINDOW_SIZE - capacity);
        }

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let resp = ui.add(
                    egui::DragValue::new(&mut self.decimation_gui)
                        .speed(1.0).range(0..=4095).clamp_existing_to_range(true)
                );

                if resp.changed() {
                    self.params.param0.value.store(self.decimation_gui, Ordering::Relaxed);
                }

                ui.separator();

                let speed = self.params.speed.load(Ordering::Relaxed) as f64;
                let (value, unit) = if speed < 1000.0 {
                    (speed, "")
                } else if speed < 1000000.0 {
                    (speed / 1000.0, "k")
                } else {
                    (speed / 1000000.0, "M")
                };
                ui.label(format!("sample freq: {:.1}{}Sps", value, unit));
            });
        });

        if !is_paused {
            if got_data {
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
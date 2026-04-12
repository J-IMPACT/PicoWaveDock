pub mod plot;
pub mod scope;
pub mod spectrum;

use std::collections::VecDeque;

use crate::data::Data;

const WINDOW_SIZE: usize = 5_000;

pub struct ViewerAxisY {
    auto_scale: bool,
    y_min: f32,
    y_max: f32,
}

impl ViewerAxisY {
    pub fn new_with_autoscale() -> Self {
        Self { auto_scale: true, y_min: -1.0, y_max: 1.0 }
    }
    pub fn new_with_min_max(y_min: f32, y_max: f32) -> Self {
        Self { auto_scale: false, y_min, y_max }
    }
    fn update<T: Data>(&mut self, samples: &VecDeque<T>) {
        if !self.auto_scale || samples.is_empty() { return; }
        let mut mn = f32::MAX;
        let mut mx = f32::MIN;
        for &v in samples {
            let vf = v.to_f32().unwrap();
            mn = mn.min(vf);
            mx = mx.max(vf);
        }

        let diff = mx - mn;
        (self.y_min, self.y_max) = if diff == 0.0 {
            (self.y_min - 1.0, self.y_max + 1.0)
        } else {
            (mn - diff * 0.1, mx + diff * 0.1)
        };
    }
}


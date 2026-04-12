use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::params::ArcParams;

pub struct SpeedMeter {
    time: Instant,
    read_count: usize,
    sample_per_byte_num: usize,
    sample_per_byte_den: usize,
}

impl SpeedMeter {
    pub fn new(
        sample_per_byte_num: usize,
        sample_per_byte_den: usize
    ) -> Self {
        let time = Instant::now();
        Self { time, read_count: 0, sample_per_byte_num, sample_per_byte_den }
    }
    pub fn run(&mut self, num_bytes: usize, params: ArcParams) {
        self.read_count += num_bytes;
        if self.time.elapsed().as_secs_f64() >= 1.0 {
            let speed = self.read_count * self.sample_per_byte_num / self.sample_per_byte_den;
            params.speed.store(speed, Ordering::Relaxed);
            self.read_count = 0;
            self.time = Instant::now();
        }
    }
}
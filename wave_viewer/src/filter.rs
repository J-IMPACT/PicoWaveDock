use std::collections::VecDeque;
use std::sync::atomic::Ordering;

use crate::data::Data;
use crate::params::ArcParams;

pub trait Filter<TI: Data, TO: Data>: Send {
    fn run(&mut self, value: TI, params: ArcParams) -> Option<TO>;
}

pub struct DecimationFilter {
    count: usize,
}

impl DecimationFilter {
    pub fn new() -> Self { Self { count: 0 } }
}

impl<T: Data> Filter<T, T> for DecimationFilter {
    fn run(&mut self, value: T, params: ArcParams) -> Option<T> {
        let decimation = params.param0.load(Ordering::Relaxed) as usize;
        if self.count >= decimation {
            self.count = 0;
            Some(value)
        } else {
            self.count += 1;
            None
        }
    }
}

pub struct MovAveFilter<T> {
    decimation_filter: DecimationFilter,
    queue: VecDeque<T>,
    sum: T,
}

impl<T: Data> MovAveFilter<T> {
    pub fn new(params: ArcParams) -> Self {
        let decimation_filter = DecimationFilter::new();
        let window = params.param1.load(Ordering::Relaxed);
        let queue = VecDeque::with_capacity(window as usize);
        Self { decimation_filter, queue, sum: T::zero() }
    }
}

impl<TI: Data, TO: Data> Filter<TI, TO> for MovAveFilter<TI> {
    fn run(&mut self, value: TI, params: ArcParams) -> Option<TO> {
        let window = params.param1.load(Ordering::Relaxed) as usize;
        while self.queue.len() >= window {
            self.sum -= self.queue.pop_front().unwrap();
        }
        self.queue.push_back(value);
        self.sum += value;
        let ave = TO::from_f64(self.sum.to_f64().unwrap() / window as f64).unwrap();
        self.decimation_filter.run(ave, params)
    }
}
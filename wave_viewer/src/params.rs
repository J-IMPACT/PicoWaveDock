use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};

pub struct Param {
    pub value: AtomicIsize,
    min: AtomicIsize,
    max: AtomicIsize,
    pub edit_name: Option<String>,
}

impl Param {
    pub fn load(&self, order: Ordering) -> isize {
        self.value.load(order)
    }
    pub fn load_min(&self, order: Ordering) -> isize {
        self.min.load(order)
    }
    pub fn load_max(&self, order: Ordering) -> isize {
        self.max.load(order)
    }
}

pub struct ParamBuilder {
    value: isize,
    min: isize,
    max: isize,
    edit_name: Option<String>,
}

impl ParamBuilder {
    fn new() -> Self {
        Self { value: 1, min: 1, max: 1, edit_name: None }
    }
    fn build(&self) -> Param {
        let value = AtomicIsize::new(self.value);
        let min = AtomicIsize::new(self.min);
        let max = AtomicIsize::new(self.max);
        let edit_name = self.edit_name.clone();
        Param { value, min, max, edit_name }
    }
    pub fn set_value_range(&mut self, value: isize, min: isize, max: isize) {
        self.value = value;
        self.min = min.min(value);
        self.max = max.max(value);
    }
}

pub struct Params {
    pub stop: AtomicBool,
    pub paused: AtomicBool,

    pub speed: AtomicUsize,

    pub param0: Param,
    pub param1: Param,
    pub param2: Param,
    pub param3: Param,
    pub param4: Param,
}

pub struct ParamsBuilder {
    pub param0: ParamBuilder,
    pub param1: ParamBuilder,
    pub param2: ParamBuilder,
    pub param3: ParamBuilder,
    pub param4: ParamBuilder,
}

impl ParamsBuilder {
    pub fn new() -> Self {
        Self {
            param0: ParamBuilder::new(),
            param1: ParamBuilder::new(),
            param2: ParamBuilder::new(),
            param3: ParamBuilder::new(),
            param4: ParamBuilder::new(),
        }
    }
    pub fn build(&self) -> ArcParams {
        Arc::new(Params {
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(true),

            speed: AtomicUsize::new(0),

            param0: self.param0.build(),
            param1: self.param1.build(),
            param2: self.param2.build(),
            param3: self.param3.build(),
            param4: self.param4.build(),
        })
    }
}

pub type ArcParams = Arc<Params>;
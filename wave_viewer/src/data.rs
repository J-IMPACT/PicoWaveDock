use num_traits::{FromPrimitive, ToPrimitive, Zero};
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Sub, SubAssign};

pub trait Data: Sized + Clone + Copy + Send 
+ Add + Sub + Mul + Div + AddAssign + SubAssign + MulAssign + DivAssign 
+ PartialEq + PartialOrd
+ Zero + ToPrimitive + FromPrimitive {}

impl Data for u8 {}
impl Data for u16 {}
impl Data for f64 {}
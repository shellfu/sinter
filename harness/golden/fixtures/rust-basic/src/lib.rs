use std::fmt;

mod util;

use crate::util::double;

pub fn scaled(x: i32) -> i32 {
    double(x)
}

/// Adds two numbers.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn compute(x: i32) -> i32 {
    add(x, 1)
}

pub struct Config {
    pub retries: u32,
}

impl Config {
    /// Creates the default config.
    pub fn new() -> Config {
        Config {
            retries: default_retries(),
        }
    }
}

fn default_retries() -> u32 {
    3
}

mod inner {
    pub fn helper() {}
}

pub enum Mode {
    Fast,
    Slow,
}

pub trait Runner {
    fn run(&self);
}

pub const MAX: u32 = 10;

type Alias = fmt::Result;

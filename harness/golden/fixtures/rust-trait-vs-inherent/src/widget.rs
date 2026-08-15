pub struct Widget;

pub trait Runner {
    fn run(&self);
}

impl Widget {
    pub fn new() -> Widget {
        Widget
    }

    pub fn run(&self) {}
}

impl Runner for Widget {
    fn run(&self) {}
}

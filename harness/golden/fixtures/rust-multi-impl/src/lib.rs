pub struct Counter {
    pub n: u32,
}

impl Counter {
    pub fn new() -> Counter {
        Counter { n: 0 }
    }
}

impl Counter {
    pub fn reset(&mut self) {
        self.n = 0;
    }
}

pub fn make() -> Counter {
    Counter::new()
}

pub mod types;

use types::Engine;

impl Engine {
    pub fn run(&self) -> u32 {
        self.run_impl(false)
    }

    fn run_impl(&self, _dry: bool) -> u32 {
        self.n
    }
}

pub struct Local {
    pub n: u32,
}

impl Local {
    pub fn outer(&self) -> u32 {
        self.inner()
    }

    fn inner(&self) -> u32 {
        self.n
    }
}

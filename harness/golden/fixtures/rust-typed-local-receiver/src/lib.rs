pub struct Dog;

impl Dog {
    pub fn speak(&self) {}
}

pub fn announce(d: &Dog) {
    d.speak();
}

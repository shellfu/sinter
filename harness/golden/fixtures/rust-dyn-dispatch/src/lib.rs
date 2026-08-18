pub trait Speak {
    fn speak(&self);
}

pub struct Dog;
pub struct Cat;

impl Speak for Dog {
    fn speak(&self) {}
}

impl Speak for Cat {
    fn speak(&self) {}
}

pub fn announce(s: &dyn Speak) {
    Speak::speak(s);
}

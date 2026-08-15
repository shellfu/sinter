mod util;

use crate::util::double;

pub fn twice(x: i32) -> i32 {
    double(x)
}

pub fn shadowed(x: i32) -> i32 {
    let double = |v: i32| v + 1;
    double(x)
}

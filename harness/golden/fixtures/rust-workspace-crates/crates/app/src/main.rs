use acme_util::double;

/// Local helper.
fn helper(x: i64) -> i64 {
    x + 1
}

fn main() {
    double(2);
    crate::helper(3);
    acme_util::double(4);
}

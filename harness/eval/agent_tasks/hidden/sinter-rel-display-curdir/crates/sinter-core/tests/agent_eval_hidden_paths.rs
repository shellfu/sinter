use sinter_core::rel_display;
use std::path::Path;
#[test]
fn curdir_dropped() {
    assert_eq!(rel_display(Path::new("./src/a.rs")), "src/a.rs");
    assert_eq!(rel_display(Path::new("src/./a.rs")), "src/a.rs");
    assert_eq!(rel_display(Path::new("src/a.rs")), "src/a.rs");
}

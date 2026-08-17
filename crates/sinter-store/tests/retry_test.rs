//! Open contention: a second open retries past a transiently held handle
//! instead of failing on first AlreadyOpen.

use sinter_store::Store;
#[test]
fn open_retries_past_transient_holder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.redb");
    Store::create(&path).unwrap();
    let held = Store::open(&path).unwrap();
    let p = path.clone();
    let t = std::thread::spawn(move || Store::open(&p).map(|_| ()).map_err(|e| e.to_string()));
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(held);
    t.join().unwrap().expect("retry should absorb a 100ms hold");
}

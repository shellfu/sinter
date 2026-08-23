use grep_matcher::ByteSet;
#[test]
fn byteset_len() {
    let mut s = ByteSet::empty();
    assert!(s.is_empty());
    assert_eq!(s.len(), 0);
    s.add(b'a');
    s.add_all(b'0', b'9');
    assert_eq!(s.len(), 11);
    s.remove(b'a');
    assert_eq!(s.len(), 10);
    assert_eq!(ByteSet::full().len(), 256);
}

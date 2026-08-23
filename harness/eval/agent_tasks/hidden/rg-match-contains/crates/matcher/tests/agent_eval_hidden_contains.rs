use grep_matcher::Match;
#[test]
fn match_contains() {
    let m = Match::new(2, 10);
    assert!(m.contains(Match::new(2, 10)));
    assert!(m.contains(Match::new(3, 9)));
    assert!(m.contains(Match::zero(2)));
    assert!(m.contains(Match::zero(10)));
    assert!(!m.contains(Match::new(1, 5)));
    assert!(!m.contains(Match::new(5, 11)));
}

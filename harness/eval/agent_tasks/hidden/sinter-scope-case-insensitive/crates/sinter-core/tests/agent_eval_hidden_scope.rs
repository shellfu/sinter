use sinter_core::CorpusScope;
#[test]
fn case_insensitive_scope() {
    assert_eq!(CorpusScope::from_str_opt("Test"), Some(CorpusScope::Test));
    assert_eq!(CorpusScope::from_str_opt("PROD"), Some(CorpusScope::Production));
    assert_eq!(CorpusScope::from_str_opt("Docs"), Some(CorpusScope::Docs));
    assert_eq!(CorpusScope::from_str_opt("nope"), None);
}

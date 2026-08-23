use sinter_core::CorpusScope;
#[test]
fn jest_dirs_are_test() {
    assert_eq!(CorpusScope::classify_path("src/__tests__/app.ts"), CorpusScope::Test);
    assert_eq!(CorpusScope::classify_path("src/__mocks__/fs.ts"), CorpusScope::Test);
    assert_eq!(CorpusScope::classify_path("src/app.ts"), CorpusScope::Production);
}

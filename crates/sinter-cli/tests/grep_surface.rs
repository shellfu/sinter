//! `sinter grep` without a traversal: the indexed corpus is the bound.

use std::path::Path;
use std::process::Command;

fn sinter(repo: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, content: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write("go.mod", "module example.com/fixture\n\ngo 1.22\n");
    write("lib.go", "package main\n\nfunc Base() int { return 1 }\n");
    write(
        "sub/util.go",
        "package sub\n\nfunc Helper() int { return 2 }\n",
    );
    write(
        "lib_test.go",
        "package main\n\nimport \"testing\"\n\nfunc TestBase(t *testing.T) { Base() }\n",
    );
    let (code, _, err) = sinter(dir.path(), &["build"]);
    assert_eq!(code, 0, "{err}");
    dir
}

#[test]
fn no_within_searches_every_indexed_file_in_scope() {
    let dir = fixture();
    let (code, out, _) = sinter(dir.path(), &["grep", "Base"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("repo-wide"), "{out}");
    assert!(
        out.contains("lib.go:3:") && out.contains("lib_test.go:5:"),
        "{out}"
    );

    let (code, out, _) = sinter(dir.path(), &["grep", "Base", "--no-tests"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("lib_test.go"), "{out}");

    let (code, out, _) = sinter(dir.path(), &["grep", "zzz_absent", "--json"]);
    assert_eq!(code, 1, "{out}");
    let value: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(value["status"], "not_proven");
    assert_eq!(value["files_in_bound"], 3, "{value}");
}

#[test]
fn a_file_bound_may_name_a_directory_and_warns_when_it_names_nothing() {
    let dir = fixture();
    let (code, out, _) = sinter(dir.path(), &["grep", "func", "--within", "file(sub)"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("bound 1 files (1 searched)"), "{out}");
    assert!(out.contains("sub/util.go:3:"), "{out}");

    let (code, out, err) = sinter(dir.path(), &["grep", "func", "--within", "file(nope.go)"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        err.contains("warning: file(nope.go): no such file or directory"),
        "{err}"
    );
    let (_, out, _) = sinter(
        dir.path(),
        &["grep", "func", "--within", "file(nope.go)", "--json"],
    );
    let value: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(
        value["warnings"][0],
        "file(nope.go): no such file or directory"
    );
}

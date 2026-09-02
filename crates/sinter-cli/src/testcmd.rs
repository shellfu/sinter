//! One runnable test command per language. `impact` and `context` both
//! render affected-test rows; the shell text lives here so they agree.

/// `language` is one of `rust`, `go`, `python`, `vitest`, `npm`; anything
/// else falls back to naming the file and test. `package` is the cargo
/// package for Rust, the package directory for Go, and unused otherwise.
/// `file` is the cargo target selector (`--test surface`) for Rust and the
/// repo-relative path for every other language.
pub fn test_command(language: &str, package: &str, file: &str, name: &str) -> String {
    match language {
        "rust" => format!("cargo test -p {package} {file} -- {name}"),
        "go" => format!(
            "go test ./{} -run '^{name}$'",
            package.trim_end_matches('/')
        ),
        "python" => format!("pytest {file}::{name}"),
        "vitest" => format!("npx vitest run {file} -t '{name}'"),
        "npm" => format!("npm test -- {file}"),
        _ => format!("{file} {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::test_command;

    #[test]
    fn renders_each_language() {
        assert_eq!(
            test_command("rust", "one", "--test surface", "works"),
            "cargo test -p one --test surface -- works"
        );
        assert_eq!(
            test_command("go", "pkg/retry", "pkg/retry/retry_test.go", "TestBackoff"),
            "go test ./pkg/retry -run '^TestBackoff$'"
        );
        assert_eq!(
            test_command("python", "", "tests/test_x.py", "TestX::test_y"),
            "pytest tests/test_x.py::TestX::test_y"
        );
        assert_eq!(
            test_command("vitest", "", "src/a.test.ts", "adds"),
            "npx vitest run src/a.test.ts -t 'adds'"
        );
        assert_eq!(
            test_command("npm", "", "src/a.test.js", "adds"),
            "npm test -- src/a.test.js"
        );
    }
}

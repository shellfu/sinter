//! Runnable test commands per ecosystem, from the file and test name the
//! graph knows. Placeholder for the shared `testcmd` boundary: the
//! signature is the contract, the command shapes are the conventional
//! runners and nothing cleverer.

/// The command that runs `test_name` in `file` (both repository-relative)
/// for `language` (`rust`, `go`, `typescript`, `javascript`, `python`).
/// An empty `test_name` runs the whole file. Unknown languages get the
/// file path back as a comment so the row still names what to run.
pub fn test_command(language: &str, package_dir: &str, file: &str, test_name: &str) -> String {
    let dir = package_dir.trim_end_matches('/');
    let dir = if dir.is_empty() { "." } else { dir };
    match language {
        "rust" => match test_name {
            "" => format!("cargo test --manifest-path {dir}/Cargo.toml"),
            name => format!("cargo test --manifest-path {dir}/Cargo.toml -- {name}"),
        },
        "go" => {
            let pkg = file
                .rsplit_once('/')
                .map_or(".".to_owned(), |(d, _)| format!("./{d}"));
            match test_name {
                "" => format!("go test {pkg}"),
                name => format!("go test {pkg} -run '^{name}$'"),
            }
        }
        "typescript" | "javascript" => match test_name {
            "" => format!("npm test --prefix {dir} -- {file}"),
            name => format!("npm test --prefix {dir} -- {file} -t '{name}'"),
        },
        "python" => match test_name {
            "" => format!("python -m pytest {file}"),
            name => format!("python -m pytest {file}::{name}"),
        },
        _ => format!("# {file}"),
    }
}

#[cfg(test)]
mod tests {
    use super::test_command;

    #[test]
    fn commands_follow_each_ecosystems_runner() {
        assert_eq!(
            test_command("go", ".", "pkg/x_test.go", "TestBase"),
            "go test ./pkg -run '^TestBase$'"
        );
        assert_eq!(test_command("go", "", "x_test.go", ""), "go test .");
        assert_eq!(
            test_command(
                "typescript",
                "packages/cli",
                "packages/cli/src/index.test.ts",
                ""
            ),
            "npm test --prefix packages/cli -- packages/cli/src/index.test.ts"
        );
        assert_eq!(
            test_command("python", "", "tests/test_x.py", "test_it"),
            "python -m pytest tests/test_x.py::test_it"
        );
        assert_eq!(
            test_command("rust", "crates/a", "crates/a/src/lib.rs", "tests::works"),
            "cargo test --manifest-path crates/a/Cargo.toml -- tests::works"
        );
        assert_eq!(
            test_command("ruby", ".", "spec/a_spec.rb", ""),
            "# spec/a_spec.rb"
        );
    }
}

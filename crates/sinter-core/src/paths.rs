use std::path::Path;

/// Render a repo-relative path with `/` separators on every platform.
///
/// `Node.file`, `FileFacts` keys, and `Reference` file fields are strings
/// compared byte-exactly (store keys, import chain walking, golden
/// expectations), so the separator must not vary by host OS.
pub fn rel_display(path: &Path) -> String {
    let mut out = String::new();
    for comp in path.components() {
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&comp.as_os_str().to_string_lossy());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::rel_display;
    use std::path::PathBuf;

    #[test]
    fn joins_components_with_forward_slash() {
        let p: PathBuf = ["src", "net", "tcp.rs"].iter().collect();
        assert_eq!(rel_display(&p), "src/net/tcp.rs");
        assert!(!rel_display(&p).contains('\\'));
    }

    #[test]
    fn single_component_unchanged() {
        assert_eq!(rel_display(&PathBuf::from("main.go")), "main.go");
    }
}

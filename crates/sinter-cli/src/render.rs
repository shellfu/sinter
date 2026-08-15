//! Shared human-output helpers: span→line, signature ellipsis, terminal
//! hyperlinks. Every human-facing verb renders through these so all verbs
//! look alike.

use std::io::IsTerminal;
use std::path::Path;

/// 1-based line of a byte offset, from the file's current content.
pub fn line_of(repo: &Path, file: &str, byte: u64) -> Option<usize> {
    let source = std::fs::read_to_string(repo.join(file)).ok()?;
    let upto = source.get(..(byte as usize).min(source.len()))?;
    Some(upto.bytes().filter(|b| *b == b'\n').count() + 1)
}

/// Middle-ellipsize past `max` chars.
pub fn ellipsize(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max || max < 5 {
        return s.to_string();
    }
    let keep = max - 1;
    let head: String = chars[..keep / 2].iter().collect();
    let tail: String = chars[chars.len() - (keep - keep / 2)..].iter().collect();
    format!("{head}…{tail}")
}

/// `file:line`, as an OSC 8 hyperlink when stdout is a terminal.
pub fn location(repo: &Path, file: &str, line: Option<usize>) -> String {
    let text = match line {
        Some(line) => format!("{file}:{line}"),
        None => file.to_string(),
    };
    if std::io::stdout().is_terminal() {
        let target = repo.join(file);
        format!(
            "\u{1b}]8;;file://{}\u{1b}\\{text}\u{1b}]8;;\u{1b}\\",
            target.display()
        )
    } else {
        text
    }
}

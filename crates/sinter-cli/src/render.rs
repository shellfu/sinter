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

/// Middle-ellipsize past `max` chars, snapping both cuts to token
/// boundaries so an identifier is never split in half (`, bac… stdout`).
/// Snapping searches at most a quarter of the window; with no separator
/// in reach it falls back to the raw cut.
pub fn ellipsize(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max || max < 5 {
        return s.to_string();
    }
    let keep = max - 1;
    let slack = (keep / 4).max(1);
    let is_sep = |c: char| matches!(c, ',' | ' ' | '(' | ')' | '{');
    let mut head_end = keep / 2;
    if let Some(i) = (head_end.saturating_sub(slack)..head_end)
        .rev()
        .find(|&i| is_sep(chars[i]))
    {
        head_end = i + 1;
    }
    let mut tail_start = chars.len() - (keep - keep / 2);
    if let Some(i) = (tail_start..(tail_start + slack).min(chars.len())).find(|&i| is_sep(chars[i]))
    {
        tail_start = (i + 1).min(chars.len());
        while tail_start < chars.len() && chars[tail_start] == ' ' {
            tail_start += 1;
        }
    }
    let head: String = chars[..head_end].iter().collect();
    let tail: String = chars[tail_start..].iter().collect();
    format!("{}…{tail}", head.trim_end())
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

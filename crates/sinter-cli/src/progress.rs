//! Phase progress for the long-running verbs. A terminal gets one
//! animated line that rewrites itself; a pipe gets one plain line per
//! phase. Silence needs no reporter: `pipeline::build` drops its phases,
//! which is how queries, hooks, and MCP stay quiet. Progress is always
//! stderr — stdout carries the answer.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::pipeline::{Phase, human_bytes};
use crate::render::count;

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: std::time::Duration = std::time::Duration::from_millis(80);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Plain,
    Animated,
}

/// A phase reporter. `start` names the work in flight, `finish` replaces
/// it with the result. Both are no-ops when silent, so the same call sites
/// serve `sinter init` and a self-syncing query.
pub struct Progress {
    mode: Mode,
    /// Also the write lock: the ticker and the foreground thread both take
    /// it before touching stderr, so a frame can never interleave a line.
    label: Arc<Mutex<String>>,
    running: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
}

impl Progress {
    /// One plain line per phase, no ticker thread.
    fn plain() -> Self {
        Self {
            mode: Mode::Plain,
            label: Arc::new(Mutex::new(String::new())),
            running: Arc::new(AtomicBool::new(false)),
            ticker: None,
        }
    }

    /// Animated when stderr is a terminal, plain lines otherwise (CI logs
    /// keep the phases without the escape sequences).
    pub fn stderr() -> Self {
        if !std::io::stderr().is_terminal() {
            return Self::plain();
        }
        let label = Arc::new(Mutex::new(String::new()));
        let running = Arc::new(AtomicBool::new(true));
        let ticker = {
            let (label, running) = (Arc::clone(&label), Arc::clone(&running));
            std::thread::spawn(move || {
                let mut frame = 0usize;
                while running.load(Ordering::Relaxed) {
                    {
                        let text = lock(&label);
                        if !text.is_empty() {
                            let mut err = std::io::stderr().lock();
                            let _ = write!(err, "\r\x1b[2K{} {text}", FRAMES[frame % FRAMES.len()]);
                            let _ = err.flush();
                        }
                    }
                    frame += 1;
                    std::thread::sleep(TICK);
                }
            })
        };
        Self {
            mode: Mode::Animated,
            label,
            running,
            ticker: Some(ticker),
        }
    }

    /// Begin a phase. The label stays on screen (spinning) until the next
    /// `start` or `finish`.
    pub fn start(&self, label: impl Into<String>) {
        match self.mode {
            Mode::Plain => eprintln!("  {}...", label.into()),
            Mode::Animated => *lock(&self.label) = label.into(),
        }
    }

    /// Replace the running phase with its result line.
    pub fn finish(&self, line: impl std::fmt::Display) {
        match self.mode {
            Mode::Plain => eprintln!("  {line}"),
            Mode::Animated => {
                let mut label = lock(&self.label);
                label.clear();
                let mut err = std::io::stderr().lock();
                let _ = writeln!(err, "\r\x1b[2K{line}");
                let _ = err.flush();
            }
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        if self.mode == Mode::Animated {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        }
    }
}

/// Render one build phase. `Ready` is the success line — everything after
/// it is maintenance, and reads that way on screen.
pub fn render(progress: &Progress, phase: Phase) {
    match phase {
        Phase::Scanning => progress.start("scanning the corpus"),
        Phase::Scanned {
            files,
            changed,
            removed,
        } => {
            if changed == 0 && removed == 0 {
                progress.start(format!("scanned {}, none changed", count(files, "file")));
            } else {
                progress.start(format!(
                    "scanned {}, {changed} changed, {removed} removed",
                    count(files, "file")
                ));
            }
        }
        Phase::Extracting { files } => {
            progress.start(format!("extracting {}", count(files, "file")))
        }
        Phase::Resolving { files } => progress.start(format!("resolving {}", count(files, "file"))),
        Phase::ScipStale => progress
            .finish("!  SCIP index is older than the newest source file — rerun `sinter scip`"),
        Phase::Ready {
            nodes,
            edges,
            elapsed,
        } => progress.finish(format!(
            "\u{2713} graph ready: {}, {} ({elapsed:.1?})",
            count(nodes as usize, "symbol"),
            count(edges as usize, "edge"),
        )),
        Phase::Compacting { before } => progress.start(format!(
            "compacting {} (optional maintenance — the graph is already saved)",
            human_bytes(before)
        )),
        Phase::Compacted { before, after } => {
            let saved = before.saturating_sub(after);
            let pct = if before > 0 {
                saved as f64 / before as f64 * 100.0
            } else {
                0.0
            };
            progress.finish(format!(
                "\u{2713} compacted: {} \u{2192} {} (-{pct:.0}%)",
                human_bytes(before),
                human_bytes(after)
            ));
        }
    }
}

/// A panicking phase must not poison progress reporting into a second
/// panic — the label is display state, and its worst stale value is one
/// wrong word on screen.
fn lock(label: &Mutex<String>) -> std::sync::MutexGuard<'_, String> {
    label.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under test stderr is a pipe, so `stderr()` must degrade to plain
    /// lines rather than emitting cursor escapes into a log.
    #[test]
    fn non_terminal_stderr_is_plain() {
        let p = Progress::stderr();
        assert!(p.mode == Mode::Plain, "expected plain mode off a terminal");
        assert!(p.ticker.is_none());
    }
}

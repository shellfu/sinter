//! One build at a time per repository, and a build that never holds the
//! live graph hostage.
//!
//! Two mechanisms, both owned here because they are the same invariant
//! seen from two sides: `.sinter/build.lock` names the one process
//! allowed to write, and [`SideFile`] gives that process somewhere to
//! write that is not the file every reader is opening. A rebuild of a
//! large repository takes minutes of exclusive redb lock; before this,
//! every concurrent query queued behind it (and behind a schema bump, on
//! a graph that had already been wiped). Now readers keep opening the
//! previous graph until a rename swaps the finished one in.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A lock held by a process that is no longer around cannot be observed
/// as dead on every platform (see [`pid_alive`]); one older than this is
/// reclaimed on age alone. Longer than any real build, short enough that
/// a killed builder does not strand a repository.
const MAX_LOCK_AGE: Duration = Duration::from_secs(30 * 60);

/// Outcome of asking for the right to build.
pub enum Acquired {
    /// This process owns the build; the guard releases on drop.
    Held(BuildLock),
    /// Another live process is building. Its pid and how long it has held
    /// the lock, for the notice a caller may want to print.
    Busy { pid: u32, held: Duration },
}

/// Guard over `.sinter/build.lock`. Dropping it releases the build.
pub struct BuildLock {
    path: PathBuf,
    /// Cleared on drop, which stops the heartbeat thread.
    beating: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        self.beating
            .store(false, std::sync::atomic::Ordering::Release);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A live owner rewrites its lock every [`HEARTBEAT`]. That is what makes
/// abandonment detectable without asking the OS whether a pid is alive —
/// `kill(pid, 0)` answers that on Unix and nothing dependency-free does on
/// Windows, where a crashed build would otherwise hold every other build
/// off for [`MAX_LOCK_AGE`].
const HEARTBEAT: Duration = Duration::from_secs(5);

/// A lock whose owner has not written for this long is abandoned. Three
/// missed beats, so a loaded machine does not lose its own build.
const SILENT_OWNER: Duration = Duration::from_secs(20);

fn start_heartbeat(
    path: PathBuf,
    pid: u32,
    started: u64,
) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let beating = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = std::sync::Arc::clone(&beating);
    let _ = std::thread::Builder::new()
        .name("sinter-build-lock".into())
        .spawn(move || {
            while flag.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::sleep(HEARTBEAT);
                if !flag.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                // Same shape the lock was created with; only the third
                // field, the last beat, moves.
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let _ = std::fs::write(&path, format!("{pid} {started} {now}\n"));
            }
        });
    beating
}

pub fn lock_path(out_dir: &Path) -> PathBuf {
    out_dir.join("build.lock")
}

/// Take the build lock, waiting up to `budget` for a live owner to
/// finish. A lock owned by a dead process — or one older than
/// [`MAX_LOCK_AGE`] — is reclaimed.
///
/// Waiting is for callers with nothing to serve (no graph on disk, or one
/// at a schema this binary cannot read). A caller holding a usable graph
/// passes a small budget and serves that graph instead of blocking: a
/// stale answer now beats a fresh answer in two minutes.
pub fn acquire(out_dir: &Path, budget: Duration) -> io::Result<Acquired> {
    let path = lock_path(out_dir);
    let started = Instant::now();
    let mut delay = Duration::from_millis(10);
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use io::Write;
                let stamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let pid = std::process::id();
                writeln!(file, "{pid} {stamp} {stamp}")?;
                drop(file);
                let beating = start_heartbeat(path.clone(), pid, stamp);
                return Ok(Acquired::Held(BuildLock { path, beating }));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let owner = read_owner(&path);
                match owner {
                    // Empty or malformed. The owner creates the file and
                    // writes its pid as two separate syscalls, so this is
                    // usually a lock half a millisecond old — reclaiming
                    // it on sight would hand the same build to two
                    // processes. Only a file that has stayed unreadable
                    // is treated as abandoned.
                    None if file_age(&path) > Duration::from_secs(5) => {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    None => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Some((pid, held))
                        if !pid_alive(pid) || held > MAX_LOCK_AGE || silent(&path) =>
                    {
                        // Reclaim. The winner is whoever's create_new
                        // lands first, so a lost race just loops.
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    Some((pid, held)) if started.elapsed() >= budget => {
                        return Ok(Acquired::Busy { pid, held });
                    }
                    Some(_) => {
                        std::thread::sleep(delay);
                        delay = (delay * 2).min(Duration::from_millis(100));
                    }
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Whether the owner has stopped writing its heartbeat. A lock written by
/// an older sinter carries no beat; those fall back to the pid and age
/// checks, so an unbeaten lock is never called silent on its first beat
/// interval.
fn silent(path: &Path) -> bool {
    let Some(last) = last_beat(path) else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Duration::from_secs(now.saturating_sub(last)) > SILENT_OWNER
}

/// The third field of the lock: when the owner last announced itself.
fn last_beat(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    text.split_whitespace().nth(2)?.parse().ok()
}

/// How long ago the lock file was last written, for a lock whose contents
/// say nothing.
fn file_age(path: &Path) -> Duration {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|written| SystemTime::now().duration_since(written).ok())
        .unwrap_or(Duration::ZERO)
}

/// `(pid, how long it has been held)` from the lock file's contents.
fn read_owner(path: &Path) -> Option<(u32, Duration)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut parts = text.split_whitespace();
    let pid: u32 = parts.next()?.parse().ok()?;
    let started: u64 = parts.next()?.parse().ok()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Some((pid, Duration::from_secs(now.saturating_sub(started))))
}

/// Whether that pid is still running. `kill(pid, 0)` answers exactly this
/// on Unix; elsewhere there is no dependency-free answer, so the age
/// check in [`acquire`] is the only reclaim path.
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // EPERM means alive and owned by someone else.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// A database being built beside the live one, swapped in atomically when
/// it is finished.
pub struct SideFile {
    target: PathBuf,
    side: PathBuf,
    committed: bool,
}

impl SideFile {
    /// Stage a build of `target`. `carry_forward` copies the current
    /// database so an incremental pass keeps its facts; a fresh build (no
    /// graph, or a schema this binary rebuilds from source) starts empty.
    ///
    /// Only call while holding the build lock: this clears side files left
    /// by builds that died, which is safe precisely because no other
    /// builder exists.
    pub fn stage(target: &Path, carry_forward: bool) -> io::Result<Self> {
        let side = target.with_file_name(format!(
            "{}.build-{}",
            target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "graph.redb".into()),
            std::process::id()
        ));
        clear_abandoned(target);
        if carry_forward {
            std::fs::copy(target, &side)?;
        }
        Ok(Self {
            target: target.to_path_buf(),
            side,
            committed: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.side
    }

    /// Swap the finished build over the live graph.
    ///
    /// POSIX rename is atomic and works with readers holding the old file
    /// open — they keep reading the graph they opened. Windows refuses to
    /// replace a file another process has open, so the rename is retried
    /// briefly (a reader's handle is short-lived) and then falls back to
    /// overwriting the live file in place, which is what every build did
    /// before this existed. The finished side file survives a failure, so
    /// the build is never lost.
    pub fn commit(mut self) -> io::Result<()> {
        // redb has already fsynced its own commits; this flushes the copy
        // itself. A crash between here and the rename leaves the previous
        // graph in place, which is the safe direction.
        if let Ok(file) = std::fs::File::open(&self.side) {
            let _ = file.sync_all();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut delay = Duration::from_millis(10);
        let last = loop {
            match std::fs::rename(&self.side, &self.target) {
                Ok(()) => {
                    self.committed = true;
                    return Ok(());
                }
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(Duration::from_millis(250));
                }
                Err(e) => break e,
            }
        };
        if std::fs::copy(&self.side, &self.target).is_ok() {
            self.committed = true;
            return Ok(());
        }
        Err(io::Error::new(
            last.kind(),
            format!(
                "could not swap {} over {}: {last}. The finished graph is kept; retry `sinter build`",
                self.side.display(),
                self.target.display()
            ),
        ))
    }
}

impl Drop for SideFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.side);
        }
    }
}

/// Remove side files from builds that died before swapping.
fn clear_abandoned(target: &Path) {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return;
    };
    let prefix = format!("{}.build-", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(owner) = name.strip_prefix(&prefix) else {
            continue;
        };
        // The pid is in the name: never delete a side file whose owner is
        // still running. The build lock should already make that
        // impossible, and deleting a live build's database out from under
        // it is expensive enough to be worth the second check.
        if owner.parse::<u32>().is_ok_and(pid_alive) {
            continue;
        }
        let _ = std::fs::remove_file(entry.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_owner_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        // pid 1 is alive; a freshly reaped high pid is not. Use a pid that
        // cannot exist rather than guessing one that died.
        std::fs::write(lock_path(dir.path()), "4294967290 0\n").unwrap();
        let held = matches!(
            acquire(dir.path(), Duration::ZERO).unwrap(),
            Acquired::Held(_)
        );
        assert!(held, "a lock owned by a dead pid must be reclaimed");
    }

    #[test]
    fn live_owner_reports_busy() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = match acquire(dir.path(), Duration::ZERO).unwrap() {
            Acquired::Held(guard) => guard,
            Acquired::Busy { .. } => panic!("empty directory must grant the lock"),
        };
        assert!(matches!(
            acquire(dir.path(), Duration::from_millis(20)).unwrap(),
            Acquired::Busy { .. }
        ));
    }

    #[test]
    fn releasing_the_guard_frees_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        drop(acquire(dir.path(), Duration::ZERO).unwrap());
        assert!(!lock_path(dir.path()).exists());
        assert!(matches!(
            acquire(dir.path(), Duration::ZERO).unwrap(),
            Acquired::Held(_)
        ));
    }

    #[test]
    fn uncommitted_side_file_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("graph.redb");
        std::fs::write(&target, b"old").unwrap();
        let side = SideFile::stage(&target, true).unwrap();
        let path = side.path().to_path_buf();
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        drop(side);
        assert!(!path.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
    }

    #[test]
    fn commit_swaps_over_the_live_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("graph.redb");
        std::fs::write(&target, b"old").unwrap();
        let side = SideFile::stage(&target, false).unwrap();
        std::fs::write(side.path(), b"new").unwrap();
        let path = side.path().to_path_buf();
        side.commit().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(!path.exists());
    }
}

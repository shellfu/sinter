use crate::hooks;

/// Onboards a repository.
pub fn run() {
    hooks::install();
}

//! GitButler integration for the code review panel.
//!
//! Detection only at this stage: a repo is considered a GitButler workspace
//! when `.git/gitbutler/but.sqlite` exists AND the `but` CLI is on PATH.
//! Per-stack and per-branch diffs are added in later commits and will shell
//! out to `but` for status / diff data.

use std::path::Path;
use std::sync::OnceLock;

/// Returns true when the repo at `repo_path` is a GitButler workspace and the
/// `but` CLI is available. Both checks are required: presence of the sqlite
/// file alone is not enough — without `but` we can't fetch lane data.
#[cfg(feature = "local_fs")]
pub fn is_gitbutler_workspace(repo_path: &Path) -> bool {
    has_gitbutler_state(repo_path) && but_cli_available()
}

#[cfg(not(feature = "local_fs"))]
pub fn is_gitbutler_workspace(_repo_path: &Path) -> bool {
    false
}

/// Cheap on-disk check for GitButler workspace state.
fn has_gitbutler_state(repo_path: &Path) -> bool {
    repo_path.join(".git").join("gitbutler").join("but.sqlite").exists()
}

/// Whether the `but` CLI exists on `PATH`. Cached after first check; PATH
/// changes during a session are uncommon and not worth re-scanning per render.
fn but_cli_available() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| which_on_path("but"))
}

fn which_on_path(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                if candidate.with_extension(ext).is_file() {
                    return true;
                }
            }
        }
    }
    false
}

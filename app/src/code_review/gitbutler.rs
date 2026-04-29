//! GitButler integration for the code review panel.
//!
//! Detection plus a thin async wrapper around the `but` CLI for fetching
//! workspace status and per-stack/per-branch diffs. Per-stack and per-branch
//! diffs are wired into `DiffStateModel::load_diffs_for_repo` in commit 2/3;
//! lane attribution metadata lands in commit 4.

use std::path::Path;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use serde::Deserialize;

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

// --- `but` CLI runner ---

/// Runs `but` with the given args in `repo_path` and returns stdout. Errors on
/// non-zero exit, mirroring `run_git_command`. `but status -j` and `but diff -j`
/// always exit 0 on success; non-zero indicates a real failure.
#[cfg(feature = "local_fs")]
async fn run_but_command(repo_path: &Path, args: &[&str]) -> Result<String> {
    use command::r#async::Command;
    use command::Stdio;

    log::debug!("[GIT OPERATION] gitbutler.rs run_but_command but {}", args.join(" "));
    let output = Command::new("but")
        .args(args)
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| anyhow!("Failed to execute `but`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("`but {}` failed: {stderr}", args.join(" ")));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// --- `but status -j` JSON shape ---

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ButStatus {
    // Used by commit 4 (lane attribution) — kept now to lock the JSON shape.
    #[serde(default)]
    #[allow(dead_code)]
    pub unassigned_changes: Vec<ButStatusChange>,
    #[serde(default)]
    pub stacks: Vec<ButStack>,
    pub merge_base: Option<ButCommitRef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ButStatusChange {
    pub file_path: String,
    #[allow(dead_code)] // Used by commit 4 for lane attribution.
    pub change_type: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ButStack {
    pub cli_id: String,
    #[serde(default)]
    pub assigned_changes: Vec<ButStatusChange>,
    #[serde(default)]
    pub branches: Vec<ButBranch>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ButBranch {
    pub cli_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ButCommitRef {
    pub commit_id: String,
}

// --- `but diff -j <target>` JSON shape ---

#[derive(Debug, Clone, Deserialize)]
pub struct ButDiff {
    #[serde(default)]
    pub changes: Vec<ButDiffEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ButDiffEntry {
    pub path: String,
    #[allow(dead_code)]
    pub status: String,
}

// --- Public API ---

/// Fetches the GitButler workspace status. Returns the parsed JSON from
/// `but status -j`.
#[cfg(feature = "local_fs")]
pub async fn fetch_status(repo_path: &Path) -> Result<ButStatus> {
    let stdout = run_but_command(repo_path, &["status", "-j"]).await?;
    serde_json::from_str(&stdout).map_err(|e| anyhow!("Failed to parse `but status -j` output: {e}"))
}

/// Fetches the diff entries for a stack or branch CLI ID. Used to enumerate
/// the file set belonging to that target — the actual unified diff is computed
/// via `git diff <merge_base> -- <files>` because naively merging per-branch
/// `but diff` hunks loses upstack-overrides-downstack semantics.
#[cfg(feature = "local_fs")]
pub async fn fetch_changes(repo_path: &Path, target_cli_id: &str) -> Result<Vec<ButDiffEntry>> {
    let stdout = run_but_command(repo_path, &["diff", "-j", "--no-tui", target_cli_id]).await?;
    let parsed: ButDiff = serde_json::from_str(&stdout)
        .map_err(|e| anyhow!("Failed to parse `but diff -j {target_cli_id}` output: {e}"))?;
    Ok(parsed.changes)
}

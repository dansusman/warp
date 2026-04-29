//! GitButler integration for the code review panel.
//!
//! Detection plus a thin async wrapper around the `but` CLI for fetching
//! workspace status and per-stack/per-branch diffs. Per-stack and per-branch
//! diffs are wired into `DiffStateModel::load_diffs_for_repo` in commit 2/3;
//! lane attribution metadata lands in commit 4.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use serde::Deserialize;

/// Returns true when the repo at `repo_path` is a GitButler workspace and the
/// `but` CLI is available. Both checks are required: presence of the sqlite
/// file alone is not enough — without `but` we can't fetch lane data.
#[cfg(feature = "local_fs")]
pub fn is_gitbutler_workspace(repo_path: &Path) -> bool {
    has_gitbutler_state(repo_path) && but_cli_path().is_some()
}

#[cfg(not(feature = "local_fs"))]
pub fn is_gitbutler_workspace(_repo_path: &Path) -> bool {
    false
}

/// Cheap on-disk check for GitButler workspace state.
fn has_gitbutler_state(repo_path: &Path) -> bool {
    repo_path.join(".git").join("gitbutler").join("but.sqlite").exists()
}

/// Resolved path to the `but` CLI, or None if not found. Cached after first
/// lookup; PATH changes during a session are uncommon and we want a stable
/// path to pass to `Command::new` (GUI-launched apps don't inherit the
/// user's shell PATH on macOS).
fn but_cli_path() -> Option<&'static PathBuf> {
    static CACHED: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHED.get_or_init(|| which_on_path("but")).as_ref()
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    // Check $PATH first, then a fallback list of common bin dirs that GUI-
    // launched macOS apps don't inherit (Finder/`open` strips PATH down to
    // the system minimum, so user-installed CLIs in homebrew or ~/.local/bin
    // are invisible without this).
    let path_dirs = std::env::var("PATH")
        .ok()
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>());
    let home = std::env::var("HOME").ok().map(std::path::PathBuf::from);
    let fallback_dirs: Vec<std::path::PathBuf> = [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .chain(home.into_iter().map(|h| h.join(".local").join("bin")))
    .collect();

    for dir in path_dirs.chain(fallback_dirs.into_iter()) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                let with_ext = candidate.with_extension(ext);
                if with_ext.is_file() {
                    return Some(with_ext);
                }
            }
        }
    }
    None
}

// --- `but` CLI runner ---

/// Runs `but` with the given args in `repo_path` and returns stdout. Errors on
/// non-zero exit, mirroring `run_git_command`. `but status -j` and `but diff -j`
/// always exit 0 on success; non-zero indicates a real failure.
#[cfg(feature = "local_fs")]
async fn run_but_command(repo_path: &Path, args: &[&str]) -> Result<String> {
    use command::r#async::Command;
    use command::Stdio;

    let but_path = but_cli_path()
        .ok_or_else(|| anyhow!("`but` CLI not found on PATH or in common bin directories"))?;
    log::debug!("[GIT OPERATION] gitbutler.rs run_but_command but {}", args.join(" "));
    let output = Command::new(but_path)
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
    /// Commits belonging to this branch, ordered newest-first. The first entry
    /// is the branch tip; the parent of the oldest entry is the branch's base
    /// (either the next-down branch's tip in the stack, or the workspace
    /// merge base for the bottom branch).
    #[serde(default)]
    pub commits: Vec<ButCommit>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ButCommit {
    pub commit_id: String,
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

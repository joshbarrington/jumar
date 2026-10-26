//! A client for the `herdr` CLI. herdr keeps its own record of the worktrees it has open
//! as workspaces, so changes to those go through herdr rather than plain git.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::Deserialize;

use crate::{Error, Result};

/// A hung server must not hang jumar. Reads are quick; changes can delete or check out a
/// whole worktree, so they get longer.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const CHANGE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct Herdr {
    bin: OsString,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: String,
    pub checkout: PathBuf,
    /// Agents running in any pane of the workspace.
    pub agents: usize,
    /// herdr's combined state of those agents.
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Working,
    /// Waiting at an approval or question.
    Blocked,
    Done,
    Idle,
    /// No agent, or one herdr cannot classify.
    #[default]
    #[serde(other)]
    Unknown,
}

/// Which worktrees herdr has open, from one session snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub workspaces: Vec<Workspace>,
}

impl State {
    #[must_use]
    pub fn workspace_for(&self, path: &Path) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.checkout == path)
    }
}

impl Default for Herdr {
    fn default() -> Self {
        Self::new()
    }
}

impl Herdr {
    /// Uses `$HERDR_BIN_PATH` when herdr provides it, else `herdr` from `PATH`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bin: std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()),
        }
    }

    #[must_use]
    pub fn with_bin(bin: impl Into<OsString>) -> Self {
        Self { bin: bin.into() }
    }

    /// True when jumar runs in a herdr pane.
    #[must_use]
    pub fn is_inside() -> bool {
        std::env::var_os("HERDR_ENV").is_some_and(|v| v == "1")
    }

    /// # Errors
    ///
    /// Fails when herdr is not installed, no server is running, or it answers oddly.
    pub fn state(&self) -> Result<State> {
        parse_snapshot(&self.call_within(&["api", "snapshot"], READ_TIMEOUT)?)
    }

    /// Opens (or goes to) the workspace for an existing worktree of `repo`. herdr finds
    /// the repo from `--cwd`, which otherwise defaults to the focused pane's directory.
    ///
    /// # Errors
    ///
    /// Fails when herdr refuses.
    pub fn open(&self, path: &Path, repo: &Path, focus: bool) -> Result<()> {
        let focus = if focus { "--focus" } else { "--no-focus" };
        let args = [
            "worktree",
            "open",
            "--cwd",
            utf8(repo)?,
            "--path",
            utf8(path)?,
            focus,
        ];
        self.call(&args).map(drop)
    }

    /// Removes the workspace's checkout and closes the workspace.
    ///
    /// # Errors
    ///
    /// Fails when herdr refuses, such as for uncommitted changes without `force`.
    pub fn remove(&self, workspace: &str, force: bool) -> Result<()> {
        let mut args = vec!["worktree", "remove", "--workspace", workspace];
        if force {
            args.push("--force");
        }
        self.call(&args).map(drop)
    }

    /// # Errors
    ///
    /// Fails when herdr refuses.
    pub fn close_workspace(&self, workspace: &str) -> Result<()> {
        self.call(&["workspace", "close", workspace]).map(drop)
    }

    /// Opens one of a plugin's panes as a popup floating over the session.
    ///
    /// # Errors
    ///
    /// Fails when herdr refuses.
    pub fn open_popup(&self, plugin: &str, entrypoint: &str) -> Result<()> {
        let args = [
            "plugin",
            "pane",
            "open",
            "--plugin",
            plugin,
            "--entrypoint",
            entrypoint,
            "--placement",
            "popup",
        ];
        self.call(&args).map(drop)
    }

    fn call(&self, args: &[&str]) -> Result<String> {
        self.call_within(args, CHANGE_TIMEOUT)
    }

    /// Runs herdr and returns its stdout. herdr reports failures as JSON on stderr.
    fn call_within(&self, args: &[&str], timeout: Duration) -> Result<String> {
        let mut command = Command::new(&self.bin);
        command.args(args).stdin(Stdio::null());
        let args = args.join(" ");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            // The receiver is gone after a timeout; nothing is left to tell.
            let _ = tx.send(command.output());
        });
        let output = rx
            .recv_timeout(timeout)
            .map_err(|_| Error::Herdr(format!("`herdr {args}` did not answer in time")))?
            .map_err(Error::HerdrSpawn)?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() {
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(Error::Herdr(format!(
                "`herdr {args}` failed: {}",
                error_message(&stderr).unwrap_or_else(|| stderr.trim().to_owned())
            )))
        }
    }
}

#[derive(Deserialize)]
struct Reply<T> {
    result: Option<T>,
}

#[derive(Deserialize)]
struct SnapshotResult {
    snapshot: Snapshot,
}

#[derive(Deserialize)]
struct Snapshot {
    #[serde(default)]
    workspaces: Vec<SnapshotWorkspace>,
    #[serde(default)]
    agents: Vec<SnapshotAgent>,
}

#[derive(Deserialize)]
struct SnapshotWorkspace {
    workspace_id: String,
    #[serde(default)]
    agent_status: AgentStatus,
    worktree: Option<SnapshotWorktree>,
}

#[derive(Deserialize)]
struct SnapshotWorktree {
    checkout_path: PathBuf,
}

#[derive(Deserialize)]
struct SnapshotAgent {
    workspace_id: String,
}

/// # Errors
///
/// Fails when the text is not a session snapshot reply.
pub fn parse_snapshot(text: &str) -> Result<State> {
    let reply: Reply<SnapshotResult> =
        serde_json::from_str(text).map_err(|err| Error::Herdr(format!("bad snapshot: {err}")))?;
    let snapshot = reply
        .result
        .ok_or_else(|| Error::Herdr("snapshot reply without a result".into()))?
        .snapshot;
    let workspaces = snapshot
        .workspaces
        .into_iter()
        .filter_map(|workspace| {
            let checkout = workspace.worktree?.checkout_path;
            Some(Workspace {
                agents: snapshot
                    .agents
                    .iter()
                    .filter(|a| a.workspace_id == workspace.workspace_id)
                    .count(),
                id: workspace.workspace_id,
                status: workspace.agent_status,
                // jumar's paths are canonical; herdr's may go through a symlink.
                checkout: fs::canonicalize(&checkout).unwrap_or(checkout),
            })
        })
        .collect();
    Ok(State { workspaces })
}

#[derive(Deserialize)]
struct ErrorReply {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: String,
}

fn error_message(stderr: &str) -> Option<String> {
    serde_json::from_str::<ErrorReply>(stderr.trim())
        .ok()
        .map(|reply| reply.error.message)
}

fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::NonUtf8Path(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAPSHOT: &str = r#"{"id":"cli:api:snapshot","result":{"type":"session_snapshot",
        "snapshot":{"version":"0.9.5","workspaces":[
            {"workspace_id":"wA","label":"k7","worktree":{"checkout_path":"/nonexistent/k7"}},
            {"workspace_id":"wB","label":"test","agent_status":"working","worktree":{"checkout_path":"/nonexistent/test"}},
            {"workspace_id":"wC","label":"scratch"}],
        "agents":[{"workspace_id":"wB","pane_id":"wB:p1"},{"workspace_id":"wB","pane_id":"wB:p2"}]}}}"#;

    #[test]
    fn parse_snapshot_should_skip_workspaces_without_a_worktree() {
        assert_eq!(parse_snapshot(SNAPSHOT).unwrap().workspaces.len(), 2);
    }

    #[test]
    fn parse_snapshot_should_count_agents_per_workspace() {
        let state = parse_snapshot(SNAPSHOT).unwrap();

        let test = state.workspace_for(Path::new("/nonexistent/test")).unwrap();
        assert_eq!((test.id.as_str(), test.agents), ("wB", 2));
    }

    #[test]
    fn parse_snapshot_should_read_workspace_agent_status() {
        let state = parse_snapshot(SNAPSHOT).unwrap();

        let test = state.workspace_for(Path::new("/nonexistent/test")).unwrap();
        assert_eq!(test.status, AgentStatus::Working);
    }

    #[test]
    fn parse_snapshot_should_treat_unrecognised_status_as_unknown() {
        let snapshot = SNAPSHOT.replace("\"working\"", "\"thinking\"");

        let state = parse_snapshot(&snapshot).unwrap();

        let test = state.workspace_for(Path::new("/nonexistent/test")).unwrap();
        assert_eq!(test.status, AgentStatus::Unknown);
    }

    #[test]
    fn parse_snapshot_should_reject_error_reply() {
        assert!(parse_snapshot(r#"{"error":{"code":"x","message":"m"}}"#).is_err());
    }

    #[test]
    fn error_message_should_read_herdr_json_error() {
        let stderr = r#"{"id":"cli","error":{"code":"server_not_running","message":"no server"}}"#;

        assert_eq!(error_message(stderr).as_deref(), Some("no server"));
    }
}

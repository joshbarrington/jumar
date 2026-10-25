use std::path::Path;

use jumar_core::git::Status;
use jumar_core::inspect::Inspection;
use jumar_core::model::{Head, Health, Worktree};
use jumar_core::safety::Tier;

pub type InspectResult = jumar_core::Result<Inspection>;

/// One Nerd Font glyph. `None` means the inspection is still running.
pub fn tier_mark(result: Option<&InspectResult>) -> &'static str {
    match result {
        None => "·",
        Some(Err(_)) => "\u{f071}",
        Some(Ok(inspection)) => tier_icon(inspection.tier),
    }
}

pub fn tier_icon(tier: Tier) -> &'static str {
    match tier {
        Tier::Safe => "\u{f058}",
        Tier::Unmerged => "\u{f0aa}",
        Tier::Dirty => "\u{f06a}",
    }
}

pub fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Shortens a path under `home` to start with `~`.
pub fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => Path::new("~").join(rest).display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

pub fn source(worktree: &Worktree) -> &str {
    worktree.source.as_deref().unwrap_or("other")
}

pub fn branch(head: Option<&Head>) -> String {
    match head {
        Some(Head::Branch(name)) => name.clone(),
        Some(Head::Detached(oid)) => format!("({})", &oid[..oid.len().min(7)]),
        None => "-".into(),
    }
}

pub fn state(worktree: &Worktree, result: Option<&InspectResult>) -> String {
    if let Some(Err(_)) = result {
        return "error".into();
    }
    match worktree.health {
        Health::Prunable => return "prunable".into(),
        Health::Orphaned => return "orphaned".into(),
        Health::Healthy => {}
    }
    let status = result
        .and_then(|r| r.as_ref().ok())
        .and_then(|i| i.status.as_ref())
        .map_or_else(String::new, status);
    match (&worktree.locked, status.as_str()) {
        (None, _) => status,
        (Some(_), "" | "clean") => "locked".into(),
        (Some(_), _) => format!("locked, {status}"),
    }
}

fn status(status: &Status) -> String {
    let counts = [
        (status.changed, "changed"),
        (status.untracked, "untracked"),
        (status.conflicted, "conflicted"),
    ];
    let mut parts: Vec<String> = counts
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, label)| format!("{n} {label}"))
        .collect();
    if status.ahead > 0 {
        parts.push(format!("↑{}", status.ahead));
    }
    if status.behind > 0 {
        parts.push(format!("↓{}", status.behind));
    }
    if parts.is_empty() {
        "clean".into()
    } else {
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_should_say_clean_when_nothing_changed() {
        assert_eq!(status(&Status::default()), "clean");
    }

    #[test]
    fn status_should_list_counts_then_divergence() {
        let value = Status {
            changed: 3,
            untracked: 1,
            ahead: 2,
            behind: 1,
            ..Status::default()
        };

        assert_eq!(status(&value), "3 changed, 1 untracked, ↑2, ↓1");
    }

    #[test]
    fn tilde_should_shorten_paths_under_home() {
        assert_eq!(
            tilde(Path::new("/h/github/x"), Path::new("/h")),
            "~/github/x"
        );
    }

    #[test]
    fn branch_should_shorten_detached_oid() {
        let head = Head::Detached("0123456789abcdef".into());

        assert_eq!(branch(Some(&head)), "(0123456)");
    }
}

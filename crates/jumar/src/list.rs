use std::sync::Arc;

use anyhow::Result;
use jumar_core::git::Status;
use jumar_core::inspect::{Inspection, inspect_all};
use jumar_core::model::{Head, Health, Worktree};
use jumar_core::safety::Tier;
use serde::Serialize;

use crate::discover;

const WORKERS: usize = 8;

#[derive(Serialize)]
struct Entry<'a> {
    #[serde(flatten)]
    worktree: &'a Worktree,
    #[serde(flatten)]
    inspection: Option<&'a Inspection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub fn run(json: bool) -> Result<()> {
    let worktrees: Arc<[Worktree]> = discover::worktrees()?.into();
    let mut results: Vec<Option<jumar_core::Result<Inspection>>> = std::iter::repeat_with(|| None)
        .take(worktrees.len())
        .collect();
    for (index, result) in inspect_all(&worktrees, WORKERS) {
        results[index] = Some(result);
    }
    let entries: Vec<Entry> = worktrees
        .iter()
        .zip(&results)
        .map(|(worktree, result)| Entry {
            worktree,
            inspection: result.as_ref().and_then(|r| r.as_ref().ok()),
            error: result
                .as_ref()
                .and_then(|r| r.as_ref().err())
                .map(ToString::to_string),
        })
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        print_table(&entries);
    }
    Ok(())
}

fn print_table(entries: &[Entry]) {
    let rows: Vec<[String; 6]> = entries
        .iter()
        .map(|e| {
            [
                tier_mark(e).to_owned(),
                e.worktree.source.clone().unwrap_or_else(|| "other".into()),
                file_name(&e.worktree.repo),
                file_name(&e.worktree.path),
                branch_text(e.worktree.head.as_ref()),
                state_text(e),
            ]
        })
        .collect();
    let mut widths = [0; 6];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    for row in &rows {
        let line: Vec<String> = row
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
    for entry in entries {
        if let Some(err) = &entry.error {
            eprintln!("jumar: {}: {err}", entry.worktree.path.display());
        }
    }
}

fn tier_mark(entry: &Entry) -> &'static str {
    match entry.inspection.map(|i| i.tier) {
        Some(Tier::Safe) => "🟢",
        Some(Tier::Unmerged) => "🟡",
        Some(Tier::Dirty) => "🔴",
        None => "⚠️",
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn branch_text(head: Option<&Head>) -> String {
    match head {
        Some(Head::Branch(name)) => name.clone(),
        Some(Head::Detached(oid)) => format!("({})", &oid[..oid.len().min(7)]),
        None => "-".into(),
    }
}

fn state_text(entry: &Entry) -> String {
    if entry.error.is_some() {
        return "error".into();
    }
    match entry.worktree.health {
        Health::Prunable => return "prunable".into(),
        Health::Orphaned => return "orphaned".into(),
        Health::Healthy => {}
    }
    entry
        .inspection
        .and_then(|i| i.status.as_ref())
        .map_or_else(String::new, status_text)
}

fn status_text(status: &Status) -> String {
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
    fn status_text_should_say_clean_when_nothing_changed() {
        assert_eq!(status_text(&Status::default()), "clean");
    }

    #[test]
    fn status_text_should_list_counts_then_divergence() {
        let status = Status {
            changed: 3,
            untracked: 1,
            ahead: 2,
            behind: 1,
            ..Status::default()
        };

        assert_eq!(status_text(&status), "3 changed, 1 untracked, ↑2, ↓1");
    }

    #[test]
    fn branch_text_should_shorten_detached_oid() {
        let head = Head::Detached("0123456789abcdef".into());

        assert_eq!(branch_text(Some(&head)), "(0123456)");
    }
}

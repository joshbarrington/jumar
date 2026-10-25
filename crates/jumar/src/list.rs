use std::sync::Arc;

use anyhow::Result;
use jumar_core::inspect::{Inspection, inspect_all};
use jumar_core::model::Worktree;
use serde::Serialize;

use crate::format::{self, InspectResult};
use crate::{discover, stdout};

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
    let scan = discover::scan(&discover::Env::load()?)?;
    for warning in &scan.warnings {
        eprintln!("jumar: {warning}");
    }
    let worktrees: Arc<[Worktree]> = scan.worktrees.into_iter().filter(|w| !w.main).collect();
    let mut results: Vec<Option<InspectResult>> = std::iter::repeat_with(|| None)
        .take(worktrees.len())
        .collect();
    for (index, result) in inspect_all(&worktrees, discover::WORKERS) {
        results[index] = Some(result);
    }

    if json {
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
        stdout::write(&format!("{}\n", serde_json::to_string_pretty(&entries)?))?;
    } else {
        stdout::write(&table(&worktrees, &results))?;
        for (worktree, result) in worktrees.iter().zip(&results) {
            if let Some(Err(err)) = result {
                eprintln!("jumar: {}: {err}", worktree.path.display());
            }
        }
    }
    Ok(())
}

fn table(worktrees: &[Worktree], results: &[Option<InspectResult>]) -> String {
    let rows: Vec<[String; 6]> = worktrees
        .iter()
        .zip(results)
        .map(|(worktree, result)| {
            [
                format::tier_mark(result.as_ref()).to_owned(),
                format::source(worktree).to_owned(),
                format::file_name(&worktree.repo),
                format::file_name(&worktree.path),
                format::branch(worktree.head.as_ref()),
                format::state(worktree, result.as_ref()),
            ]
        })
        .collect();
    let mut widths = [0; 6];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut text = String::new();
    for row in &rows {
        let line: Vec<String> = row
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        text.push_str(line.join("  ").trim_end());
        text.push('\n');
    }
    text
}

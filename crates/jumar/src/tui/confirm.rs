use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use jumar_core::apply::{self, Blocked, Start, Step};
use jumar_core::editor::Key;
use jumar_core::herdr::Herdr;
use jumar_core::model::{Head, Health, Worktree};
use jumar_core::plan::Operation;
use jumar_core::safety::Tier;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{hints, key_chip, mode_tag, popup, theme, tier_color, to_u16};
use crate::format;

type Item = Result<Step, Blocked>;

/// How many `git status --short` lines to show under a dirty remove.
const CHANGES_SHOWN: usize = 5;

pub enum Decision {
    Run,
    Cancel,
}

/// The `:w` review: what will happen, with force and branch deletion toggles per remove.
pub struct Confirm {
    worktrees: Arc<[Worktree]>,
    home: PathBuf,
    items: Vec<Item>,
    selected: usize,
    quit_after: bool,
}

impl Confirm {
    pub fn new(worktrees: Arc<[Worktree]>, items: Vec<Item>, quit_after: bool) -> Self {
        Self {
            worktrees,
            home: std::env::home_dir().unwrap_or_default(),
            items,
            selected: 0,
            quit_after,
        }
    }

    pub fn handle(&mut self, key: Key) -> Option<Decision> {
        let selected = self.items.get_mut(self.selected);
        match key {
            Key::Char('j') | Key::Down => {
                self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1));
            }
            Key::Char('k') | Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Char('f') => {
                if let Some(Ok(Step::Remove(remove))) = selected {
                    remove.force = !remove.force;
                }
            }
            Key::Char('D') => {
                if let Some(Ok(Step::Remove(remove))) = selected
                    && remove.branch().is_some()
                {
                    remove.delete_branch = !remove.delete_branch;
                }
            }
            Key::Char('y') => return Some(Decision::Run),
            Key::Char('q') | Key::Esc | Key::Ctrl('c') => return Some(Decision::Cancel),
            _ => {}
        }
        None
    }

    /// Starts running every runnable step; the rest are reported as skipped.
    pub fn run(self) -> Report {
        let mut steps = Vec::new();
        let states = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| match item {
                Err(blocked) => State::Skipped(blocked.reason.clone()),
                Ok(Step::Remove(remove)) if !remove.is_runnable() => {
                    State::Skipped(if remove.agents() > 0 {
                        "agent running, not forced".into()
                    } else if remove.worktree.locked.is_some() {
                        "locked, not forced".into()
                    } else {
                        "uncommitted changes, not forced".into()
                    })
                }
                Ok(step) => {
                    steps.push((index, step.clone()));
                    State::Pending
                }
            })
            .collect();
        Report {
            remaining: steps.len(),
            results: apply::execute_all(steps, Herdr::new()),
            worktrees: self.worktrees,
            items: self.items,
            states,
            quit_after: self.quit_after,
        }
    }

    pub fn render(&self, frame: &mut Frame) {
        let mut lines = Vec::new();
        let mut selected_line = 0;
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 {
                lines.push(Line::default());
            }
            let is_selected = index == self.selected;
            if is_selected {
                selected_line = lines.len();
            }
            let marker = if is_selected {
                Span::raw("▸ ").fg(theme::YELLOW)
            } else {
                Span::raw("  ")
            };
            let mut line = heading(item, &self.worktrees, marker);
            if let Some(outcome) = outcome(item) {
                line.push_span(Span::raw("   "));
                line.push_span(outcome);
            }
            lines.push(if is_selected {
                line.bg(theme::BG1)
            } else {
                line
            });
            lines.extend(details(item, &self.home));
        }

        let mut footer = vec![mode_tag("CONFIRM", theme::YELLOW)];
        footer.extend(hints([
            ("y", "apply"),
            ("q", "back"),
            ("f", "force"),
            ("D", "delete branch"),
        ]));
        let footer = Line::from(footer);
        // The summary stays put below the scrolling list, so it is sized in but drawn apart.
        let summary = summary(&self.items);
        lines.extend([Line::default(), summary.clone()]);
        let (area, status) = popup(frame, "review", &lines, &footer);
        lines.truncate(lines.len() - 2);
        let [list, _, summary_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        let height = usize::from(list.height);
        let offset = (selected_line + 1).saturating_sub(height);
        frame.render_widget(Paragraph::new(lines).scroll((to_u16(offset), 0)), list);
        frame.render_widget(summary, summary_area);
        frame.render_widget(footer, status);
    }
}

enum State {
    Pending,
    /// With notes on what went less well than planned.
    Done(Vec<String>),
    Failed(String),
    Skipped(String),
}

/// Progress and results of a confirmed `:w`.
pub struct Report {
    worktrees: Arc<[Worktree]>,
    items: Vec<Item>,
    states: Vec<State>,
    results: Receiver<(usize, jumar_core::Result<Vec<String>>)>,
    remaining: usize,
    pub quit_after: bool,
}

impl Report {
    pub fn is_running(&self) -> bool {
        self.remaining > 0
    }

    pub fn receive(&mut self) {
        for (index, result) in self.results.try_iter() {
            self.states[index] = match result {
                Ok(notes) => State::Done(notes),
                Err(err) => State::Failed(err.to_string()),
            };
            self.remaining -= 1;
        }
    }

    pub fn render(&self, frame: &mut Frame) {
        let mut lines = Vec::new();
        for (item, state) in self.items.iter().zip(&self.states) {
            let marker = match state {
                State::Pending => Span::raw("· ").fg(theme::GRAY),
                State::Done(_) => Span::raw("✓ ").fg(theme::GREEN),
                State::Failed(_) => Span::raw("✗ ").fg(theme::RED),
                State::Skipped(_) => Span::raw("- ").fg(theme::GRAY),
            };
            lines.push(heading(item, &self.worktrees, marker));
            match state {
                State::Failed(reason) => lines.push(detail(reason).fg(theme::RED)),
                State::Skipped(reason) => lines.push(detail(&format!("skipped: {reason}"))),
                State::Done(notes) => {
                    lines.extend(notes.iter().map(|n| detail(n).fg(theme::YELLOW)));
                }
                State::Pending => {}
            }
        }

        let count = |f: fn(&State) -> bool| self.states.iter().filter(|s| f(s)).count();
        let footer = if self.is_running() {
            let total = count(|s| !matches!(s, State::Skipped(_)));
            Line::from(vec![
                mode_tag("APPLYING", theme::YELLOW),
                Span::raw(format!("  {}/{total}", total - self.remaining)),
            ])
        } else {
            let failed = count(|s| matches!(s, State::Failed(_)));
            let skipped = count(|s| matches!(s, State::Skipped(_)));
            let done = count(|s| matches!(s, State::Done(_)));
            let next = if self.quit_after { "quit" } else { "reload" };
            let mut footer = vec![mode_tag(
                "DONE",
                if failed > 0 { theme::RED } else { theme::GREEN },
            )];
            let tally: Vec<String> = [(done, "done"), (failed, "failed"), (skipped, "skipped")]
                .into_iter()
                .filter(|&(n, label)| n > 0 || label == "done")
                .map(|(n, label)| format!("{n} {label}"))
                .collect();
            footer.push(Span::raw(format!("  {}", tally.join(", "))));
            footer.extend(hints([("any key", next)]));
            Line::from(footer)
        };
        let (list, status) = popup(frame, "apply", &lines, &footer);
        let height = usize::from(list.height);
        let offset = lines.len().saturating_sub(height);
        frame.render_widget(Paragraph::new(lines).scroll((to_u16(offset), 0)), list);
        frame.render_widget(footer, status);
    }
}

/// One sentence of what `y` does with the options as they are now.
fn summary(items: &[Item]) -> Line<'static> {
    let (mut removes, mut deletes, mut keeps, mut moves, mut creates, mut skips) =
        (0, 0, 0, 0, 0, 0);
    for item in items {
        match item {
            Ok(Step::Remove(remove)) if remove.is_runnable() => {
                removes += 1;
                match (remove.branch(), remove.delete_branch) {
                    (Some(_), true) => deletes += 1,
                    (Some(_), false) => keeps += 1,
                    (None, _) => {}
                }
            }
            Ok(Step::Move(_)) => moves += 1,
            Ok(Step::Create(_)) => creates += 1,
            Ok(Step::Remove(_)) | Err(_) => skips += 1,
        }
    }
    let parts: Vec<String> = [
        (removes, "remove", "worktree"),
        (deletes, "delete", "branch"),
        (keeps, "keep", "branch"),
        (moves, "move", "worktree"),
        (creates, "create", "worktree"),
    ]
    .into_iter()
    .filter(|(n, _, _)| *n > 0)
    .map(|(n, verb, noun)| format!("{verb} {}", plural(n, noun)))
    .collect();
    let mut spans = vec![key_chip("y"), Span::raw(" will ").bold()];
    if parts.is_empty() {
        spans.push(Span::raw("do nothing").bold());
    } else {
        spans.push(Span::raw(parts.join(", ")).bold());
    }
    if skips > 0 {
        spans.push(
            Span::raw(format!(", and skip {skips}"))
                .fg(theme::RED)
                .bold(),
        );
    }
    Line::from(spans)
}

fn plural(n: usize, noun: &str) -> String {
    match (n, noun) {
        (1, _) => format!("1 {noun}"),
        (_, "branch") => format!("{n} branches"),
        _ => format!("{n} {noun}s"),
    }
}

fn heading(item: &Item, worktrees: &[Worktree], marker: Span<'static>) -> Line<'static> {
    let mut spans = vec![marker];
    match item {
        Ok(Step::Remove(remove)) => {
            spans.push(verb("REMOVE", theme::RED));
            spans.push(Span::raw(format::tier_icon(remove.tier)).fg(tier_color(remove.tier)));
            spans.push(Span::raw(" "));
            spans.push(Span::raw(label(&remove.worktree)).bold());
        }
        Ok(Step::Move(step)) => {
            let (name, subject) = move_subject(
                &step.worktree,
                step.to.as_deref(),
                step.rename_branch.as_deref(),
            );
            spans.push(verb(name, theme::YELLOW));
            spans.push(Span::raw("  "));
            spans.push(Span::raw(subject).bold());
        }
        Ok(Step::Create(create)) => {
            spans.push(verb("CREATE", theme::GREEN));
            spans.push(Span::raw("  "));
            spans.push(
                Span::raw(format!(
                    "{}/{}",
                    format::file_name(&create.repo),
                    format::file_name(&create.path)
                ))
                .bold(),
            );
        }
        Err(blocked) => {
            let (name, subject) = match &blocked.operation {
                Operation::Remove { id } => ("REMOVE", label(&worktrees[id.0])),
                Operation::Move {
                    id,
                    to,
                    rename_branch,
                } => move_subject(&worktrees[id.0], to.as_deref(), rename_branch.as_deref()),
                Operation::Create { target, .. } => ("CREATE", target.to_string()),
            };
            spans.push(verb(name, theme::GRAY));
            spans.push(Span::raw("  "));
            spans.push(Span::raw(subject).fg(theme::GRAY));
        }
    }
    Line::from(spans)
}

/// What becomes of the item, when that is not simply "it runs".
fn outcome(item: &Item) -> Option<Span<'static>> {
    match item {
        Ok(Step::Remove(remove)) if !remove.is_runnable() => {
            Some(Span::raw("skipped unless forced").fg(theme::RED))
        }
        Ok(Step::Remove(remove)) if remove.needs_force() => {
            Some(Span::raw("FORCED").fg(theme::RED).bold())
        }
        Err(_) => Some(Span::raw("can't run").fg(theme::RED)),
        Ok(_) => None,
    }
}

fn details(item: &Item, home: &Path) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match item {
        Ok(Step::Remove(remove)) => return remove_details(remove, home),
        Ok(Step::Move(step)) => {
            let folder = match &step.to {
                Some(to) => format!("moves to {}", format::tilde(to, home)),
                None => format!("stays at {}", format::tilde(&step.worktree.path, home)),
            };
            lines.push(field("folder", folder));
            lines.push(spans_field(
                "branch",
                match (&step.rename_branch, step.worktree.head.as_ref()) {
                    (Some(new), _) => vec![Span::raw("renamed to "), code(new)],
                    (None, Some(Head::Branch(name))) => vec![code(name), Span::raw(", unchanged")],
                    (None, _) => vec![Span::raw("none, the checkout is detached")],
                },
            ));
            if let Some(workspace) = &step.workspace {
                lines.push(field(
                    "herdr",
                    format!("closes workspace {workspace} and reopens it at the new folder"),
                ));
            }
        }
        Ok(Step::Create(create)) => {
            lines.push(field(
                "folder",
                format!(
                    "creates {} ({})",
                    format::tilde(&create.path, home),
                    create.source
                ),
            ));
            let branch = code(&create.branch);
            lines.push(spans_field(
                "branch",
                match &create.start {
                    Start::Existing => vec![Span::raw("checks out "), branch],
                    Start::From(_)
                        if let Some(fetch) = &create.fetch
                            && let Some(number) = fetch.pull =>
                    {
                        vec![
                            branch,
                            Span::raw(format!(
                                " from PR #{number} on {}, fetched first",
                                fetch.remote
                            )),
                        ]
                    }
                    Start::From(rev) | Start::Tracking(rev) => {
                        let how = match create.start {
                            Start::Tracking(_) => " tracking ",
                            _ => " from ",
                        };
                        let mut spans = vec![branch, Span::raw(how), code(rev)];
                        if create.fetch.is_some() {
                            spans.push(Span::raw(", fetched first"));
                        }
                        spans
                    }
                },
            ));
            if !create.copy.is_empty() {
                let files: Vec<String> = create
                    .copy
                    .iter()
                    .map(|f| f.display().to_string())
                    .collect();
                lines.push(field("copy", files.join(", ")));
            }
            for command in &create.on_create {
                lines.push(field("run", command.clone()));
            }
            if create.open_in_herdr {
                lines.push(field("herdr", "opens it in a new workspace".into()));
            }
        }
        Err(blocked) => lines.push(field("", blocked.reason.clone()).fg(theme::RED)),
    }
    lines
}

fn remove_details(remove: &apply::Remove, home: &Path) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let worktree = &remove.worktree;
    lines.push(if worktree.health == Health::Healthy {
        field(
            "folder",
            format!("deletes {}", format::tilde(&worktree.path, home)),
        )
    } else {
        field("folder", "already gone; clears git's record of it".into())
    });
    if remove.tier == Tier::Dirty {
        let n = remove.changes.len();
        lines.push(field(
            "",
            format!("{} lost if forced:", plural(n, "uncommitted change")),
        ));
        lines.extend(
            remove
                .changes
                .iter()
                .take(CHANGES_SHOWN)
                .map(|change| field("", format!("  {change}")).fg(theme::GRAY)),
        );
        if n > CHANGES_SHOWN {
            lines.push(field("", format!("  … {} more", n - CHANGES_SHOWN)).fg(theme::GRAY));
        }
    }
    if let Some(workspace) = &remove.workspace {
        lines.push(field(
            "herdr",
            match workspace.agents {
                0 => format!("closes workspace {}", workspace.id),
                n => format!(
                    "closes workspace {}; {} running, stopped if forced",
                    workspace.id,
                    plural(n, "agent")
                ),
            },
        ));
    }
    match remove.branch() {
        Some(branch) => {
            let note = match remove.tier {
                Tier::Safe => "merged or pushed, nothing is lost",
                Tier::Unmerged if remove.upstream_gone => {
                    "upstream gone, so probably merged; if not, deleting loses its commits"
                }
                Tier::Unmerged => "has commits found nowhere else; deleting loses them",
                Tier::Dirty => "may have commits found nowhere else",
            };
            let danger = remove.delete_branch && remove.tier != Tier::Safe;
            lines.push(option(
                remove.delete_branch,
                'D',
                format!("delete branch {branch}"),
                note,
                danger,
            ));
        }
        None => lines.push(field("branch", "none, the checkout is detached".into())),
    }
    if let Some(reason) = &worktree.locked {
        lines.push(field(
            "lock",
            match reason.as_str() {
                "" => "locked; unlocked if forced".into(),
                reason => format!("locked ({reason}); unlocked if forced"),
            },
        ));
    }
    if remove.needs_force() {
        let mut what = Vec::new();
        if worktree.locked.is_some() {
            what.push("unlock".to_owned());
        }
        if remove.tier == Tier::Dirty {
            what.push(format!(
                "discard {}",
                plural(remove.changes.len(), "uncommitted change")
            ));
        }
        if remove.agents() > 0 {
            what.push(format!("stop {}", plural(remove.agents(), "agent")));
        }
        lines.push(option(
            remove.force,
            'f',
            format!("force: {}", what.join(" and ")),
            "",
            remove.force,
        ));
    }
    lines
}

fn field(label: &str, text: String) -> Line<'static> {
    spans_field(label, vec![Span::raw(text)])
}

fn spans_field(label: &str, text: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::raw(format!("    {label:<8}  ")).fg(theme::GRAY)];
    spans.extend(text);
    Line::from(spans)
}

/// A branch or revision, set off like inline code.
fn code(text: &str) -> Span<'static> {
    Span::raw(format!(" {text} ")).fg(theme::TAN).bg(theme::BG1)
}

/// A toggle on the selected item: its box, the key that flips it, and what it does.
fn option(checked: bool, key: char, text: String, note: &str, danger: bool) -> Line<'static> {
    let mark = if checked { "[x]" } else { "[ ]" };
    let style = match (checked, danger) {
        (true, true) => Style::new().fg(theme::RED).bold(),
        (true, false) => Style::new().fg(theme::GREEN),
        (false, _) => Style::new().fg(theme::GRAY),
    };
    let mut spans = vec![
        Span::raw("    "),
        Span::styled(format!("{mark} "), style),
        Span::raw(format!("{key}")).fg(theme::YELLOW).bold(),
        Span::raw("  "),
        Span::styled(text, if checked { style } else { Style::new() }),
    ];
    if !note.is_empty() {
        spans.push(Span::raw(format!("  · {note}")).fg(theme::GRAY));
    }
    Line::from(spans)
}

/// A line under a heading in the apply report.
fn detail(text: &str) -> Line<'static> {
    Line::from(format!("            {text}"))
}

/// The verb and `old → new` of a move: its directory, or its branch when only that changes.
fn move_subject(
    worktree: &Worktree,
    to: Option<&Path>,
    branch: Option<&str>,
) -> (&'static str, String) {
    match (to, &worktree.head) {
        (Some(to), _) => (
            "MOVE",
            format!("{} → {}", label(worktree), format::file_name(to)),
        ),
        (None, Some(Head::Branch(old))) => (
            "RENAME",
            format!(
                "{}  {old} → {}",
                label(worktree),
                branch.unwrap_or_default()
            ),
        ),
        (None, _) => ("RENAME", label(worktree)),
    }
}

fn verb(name: &'static str, color: Color) -> Span<'static> {
    Span::styled(format!("{name:<8}"), Style::new().fg(color).bold())
}

fn label(worktree: &Worktree) -> String {
    format!(
        "{}/{}",
        format::file_name(&worktree.repo),
        format::file_name(&worktree.path)
    )
}

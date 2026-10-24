use std::sync::Arc;
use std::sync::mpsc::Receiver;

use jumar_core::apply::{self, Blocked, Start, Step};
use jumar_core::editor::Key;
use jumar_core::herdr::Herdr;
use jumar_core::model::Worktree;
use jumar_core::plan::Operation;
use jumar_core::safety::Tier;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{mode_tag, to_u16};
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
    items: Vec<Item>,
    selected: usize,
    quit_after: bool,
}

impl Confirm {
    pub fn new(worktrees: Arc<[Worktree]>, items: Vec<Item>, quit_after: bool) -> Self {
        Self {
            worktrees,
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
            Key::Char('n' | 'q') | Key::Esc | Key::Ctrl('c') => return Some(Decision::Cancel),
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
        let [list, status] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        let mut lines = Vec::new();
        let mut selected_line = 0;
        for (index, item) in self.items.iter().enumerate() {
            let is_selected = index == self.selected;
            if is_selected {
                selected_line = lines.len();
            }
            let marker = if is_selected { "▸ " } else { "  " };
            let line = describe(item, &self.worktrees, Span::raw(marker));
            lines.push(if is_selected {
                line.bg(Color::Indexed(236))
            } else {
                line
            });
            if let Ok(Step::Remove(remove)) = item {
                lines.extend(changes(remove));
            }
        }
        let height = usize::from(list.height);
        let offset = (selected_line + 1).saturating_sub(height);
        frame.render_widget(Paragraph::new(lines).scroll((to_u16(offset), 0)), list);

        let footer = Line::from(vec![
            mode_tag("CONFIRM", Color::Yellow),
            Span::raw("  y apply  n back  f force  D toggle branch deletion"),
        ]);
        frame.render_widget(footer, status);
    }
}

enum State {
    Pending,
    Done,
    Failed(String),
    Skipped(String),
}

/// Progress and results of a confirmed `:w`.
pub struct Report {
    worktrees: Arc<[Worktree]>,
    items: Vec<Item>,
    states: Vec<State>,
    results: Receiver<(usize, jumar_core::Result<()>)>,
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
                Ok(()) => State::Done,
                Err(err) => State::Failed(err.to_string()),
            };
            self.remaining -= 1;
        }
    }

    pub fn render(&self, frame: &mut Frame) {
        let [list, status] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        let mut lines = Vec::new();
        for (item, state) in self.items.iter().zip(&self.states) {
            let marker = match state {
                State::Pending => Span::raw("· ").dark_gray(),
                State::Done => Span::raw("✓ ").green(),
                State::Failed(_) => Span::raw("✗ ").red(),
                State::Skipped(_) => Span::raw("- ").dark_gray(),
            };
            lines.push(describe(item, &self.worktrees, marker));
            match state {
                State::Failed(reason) => lines.push(detail(reason).red()),
                State::Skipped(reason) => lines.push(detail(&format!("skipped: {reason}"))),
                State::Pending | State::Done => {}
            }
        }
        let height = usize::from(list.height);
        let offset = lines.len().saturating_sub(height);
        frame.render_widget(Paragraph::new(lines).scroll((to_u16(offset), 0)), list);

        let count = |f: fn(&State) -> bool| self.states.iter().filter(|s| f(s)).count();
        let footer = if self.is_running() {
            let total = count(|s| !matches!(s, State::Skipped(_)));
            Line::from(vec![
                mode_tag("APPLYING", Color::Blue),
                Span::raw(format!("  {}/{total}", total - self.remaining)),
            ])
        } else {
            let failed = count(|s| matches!(s, State::Failed(_)));
            let skipped = count(|s| matches!(s, State::Skipped(_)));
            let done = count(|s| matches!(s, State::Done));
            let next = if self.quit_after { "quit" } else { "reload" };
            Line::from(vec![
                mode_tag("DONE", if failed > 0 { Color::Red } else { Color::Green }),
                Span::raw(format!(
                    "  {done} done, {failed} failed, {skipped} skipped. Press any key to {next}"
                )),
            ])
        };
        frame.render_widget(footer, status);
    }
}

fn describe<'a>(item: &'a Item, worktrees: &[Worktree], marker: Span<'a>) -> Line<'a> {
    let mut spans = vec![marker];
    match item {
        Ok(Step::Remove(remove)) => {
            spans.push(verb("REMOVE", Color::Red));
            spans.push(Span::raw(label(&remove.worktree)));
            spans.push(Span::raw("  "));
            spans.push(Span::raw(format::tier_emoji(remove.tier)));
            spans.push(Span::raw(" "));
            spans.push(Span::raw(match (remove.branch(), remove.delete_branch) {
                (None, _) => "detached".to_owned(),
                (Some(branch), true) => format!("deletes branch {branch}"),
                (Some(branch), false) => format!("keeps branch {branch}"),
            }));
            if remove.tier == Tier::Dirty {
                let n = remove.changes.len();
                spans.push(Span::raw("  "));
                spans.push(if remove.force {
                    Span::raw(format!("FORCE: discards {n} change(s)"))
                        .red()
                        .bold()
                } else {
                    Span::raw(format!("{n} uncommitted change(s), f to force")).red()
                });
            }
            if let Some(workspace) = &remove.workspace {
                spans.push(
                    Span::raw(format!("  closes herdr workspace {}", workspace.id)).dark_gray(),
                );
                if workspace.agents > 0 {
                    let n = workspace.agents;
                    spans.push(Span::raw("  "));
                    spans.push(if remove.force {
                        Span::raw(format!("FORCE: stops {n} agent(s)")).red().bold()
                    } else {
                        Span::raw(format!("{n} agent(s) running, f to force")).red()
                    });
                }
            }
        }
        Ok(Step::Move(step)) => {
            spans.push(verb("MOVE", Color::Yellow));
            spans.push(Span::raw(format!(
                "{} → {}",
                label(&step.worktree),
                format::file_name(&step.to)
            )));
            if let Some(branch) = &step.rename_branch {
                spans.push(Span::raw(format!("  renames branch to {branch}")).dark_gray());
            }
            if let Some(workspace) = &step.workspace {
                spans.push(
                    Span::raw(format!("  closes and reopens herdr workspace {workspace}"))
                        .dark_gray(),
                );
            }
        }
        Ok(Step::Create(create)) => {
            spans.push(verb("CREATE", Color::Green));
            spans.push(Span::raw(format!(
                "{}/{}",
                format::file_name(&create.repo),
                create.branch
            )));
            let start = match &create.start {
                Start::Existing => "existing branch".to_owned(),
                Start::From(base) => format!("new branch from {base}"),
            };
            let herdr = if create.open_in_herdr {
                ", opens in herdr"
            } else {
                ""
            };
            spans.push(Span::raw(format!("  {}, {start}{herdr}", create.source)).dark_gray());
        }
        Err(blocked) => {
            let (name, subject) = match &blocked.operation {
                Operation::Remove { id } => ("REMOVE", label(&worktrees[id.0])),
                Operation::Move { id, to, .. } => (
                    "MOVE",
                    format!("{} → {}", label(&worktrees[id.0]), format::file_name(to)),
                ),
                Operation::Create { target, .. } => ("CREATE", target.clone()),
            };
            spans.push(verb(name, Color::DarkGray));
            spans.push(Span::raw(subject).dark_gray());
            spans.push(Span::raw(format!("  ✗ {}", blocked.reason)).red());
        }
    }
    Line::from(spans)
}

fn changes(remove: &apply::Remove) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = remove
        .changes
        .iter()
        .take(CHANGES_SHOWN)
        .map(|change| detail(change).dark_gray())
        .collect();
    if remove.changes.len() > CHANGES_SHOWN {
        let more = remove.changes.len() - CHANGES_SHOWN;
        lines.push(detail(&format!("… {more} more")).dark_gray());
    }
    lines
}

fn detail(text: &str) -> Line<'static> {
    Line::from(format!("            {text}"))
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

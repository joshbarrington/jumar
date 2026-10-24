use std::sync::Arc;
use std::sync::mpsc::Receiver;

use jumar_core::editor::{Editor, Entry, Mode};
use jumar_core::inspect::inspect_all;
use jumar_core::model::{Health, Worktree};
use jumar_core::plan::Line as BufferLine;
use jumar_core::safety::Tier;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{column_width, mode_tag, pad, to_u16, width};
use crate::discover::{self, Scan};
use crate::format::{self, InspectResult};

/// The editable worktree list, with inspection results filling in as they arrive.
pub struct Buffer {
    pub worktrees: Arc<[Worktree]>,
    pub editor: Editor,
    results: Vec<Option<InspectResult>>,
    loading: usize,
    inspections: Receiver<(usize, InspectResult)>,
    scroll: usize,
}

/// Display text for one cell of each column, computed once per frame.
struct Row<'a> {
    mark: &'static str,
    repo: String,
    name: &'a str,
    branch: String,
    state: String,
    source: &'a str,
    style: Style,
    state_style: Style,
}

impl Buffer {
    pub fn new(scan: Scan) -> Self {
        let worktrees: Arc<[Worktree]> = scan.worktrees.into();
        let entries = worktrees
            .iter()
            .map(|w| Entry {
                name: format::file_name(&w.path),
                keywords: format!(
                    "{} {} {}",
                    format::file_name(&w.repo),
                    format::source(w),
                    format::branch(w.head.as_ref()),
                ),
            })
            .collect();
        let mut editor = Editor::new(entries);
        if let Some(warning) = scan.warnings.first() {
            editor.set_message(warning.clone());
        }
        Self {
            results: std::iter::repeat_with(|| None)
                .take(worktrees.len())
                .collect(),
            loading: worktrees.len(),
            inspections: inspect_all(&worktrees, discover::WORKERS),
            editor,
            worktrees,
            scroll: 0,
        }
    }

    pub fn is_loading(&self) -> bool {
        self.loading > 0
    }

    pub fn receive(&mut self) {
        for (index, result) in self.inspections.try_iter() {
            self.results[index] = Some(result);
            self.loading -= 1;
        }
    }

    /// Tiers known so far, by entry.
    pub fn tiers(&self) -> Vec<Option<Tier>> {
        self.results
            .iter()
            .map(|r| r.as_ref().and_then(|r| r.as_ref().ok()).map(|i| i.tier))
            .collect()
    }

    fn rows(&self) -> Vec<Row<'_>> {
        self.editor
            .rows()
            .map(|(_, line)| match line {
                BufferLine::Existing { id, name } => {
                    let worktree = &self.worktrees[id.0];
                    let result = self.results[id.0].as_ref();
                    let renamed =
                        Some(name.as_str()) != worktree.path.file_name().and_then(|n| n.to_str());
                    Row {
                        mark: format::tier_mark(result),
                        repo: format::file_name(&worktree.repo),
                        name,
                        branch: format::branch(worktree.head.as_ref()),
                        state: format::state(worktree, result),
                        source: format::source(worktree),
                        style: if renamed {
                            Style::new().fg(Color::Yellow)
                        } else {
                            Style::new()
                        },
                        state_style: match (worktree.health, result) {
                            (Health::Prunable | Health::Orphaned, _) | (_, Some(Err(_))) => {
                                Style::new().fg(Color::Red)
                            }
                            _ => Style::new().fg(Color::DarkGray),
                        },
                    }
                }
                BufferLine::New(text) => Row {
                    mark: "+ ",
                    repo: String::new(),
                    name: text,
                    branch: String::new(),
                    state: String::new(),
                    source: "",
                    style: Style::new().fg(Color::Green),
                    state_style: Style::new(),
                },
            })
            .collect()
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let [list, status] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        let (row, col) = self.editor.cursor();
        let cursor_pos = self.editor.rows().position(|(index, _)| index == row);
        let height = usize::from(list.height);
        if let Some(pos) = cursor_pos {
            if pos < self.scroll {
                self.scroll = pos;
            } else if pos >= self.scroll + height {
                self.scroll = pos + 1 - height;
            }
        }
        let rows = self.rows();

        let repo_width = column_width(rows.iter().map(|r| r.repo.as_str()));
        let name_width = column_width(rows.iter().map(|r| r.name));
        let branch_width = column_width(rows.iter().map(|r| r.branch.as_str()));
        let lines: Vec<Line> = rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(height)
            .map(|(pos, r)| {
                let line = Line::from(vec![
                    Span::raw(r.mark),
                    Span::raw(" "),
                    Span::raw(pad(&r.repo, repo_width)).fg(Color::Blue),
                    Span::raw("  "),
                    Span::styled(
                        pad(r.name, name_width),
                        r.style.add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::raw(pad(&r.branch, branch_width)).fg(Color::Magenta),
                    Span::raw("  "),
                    Span::styled(r.state.clone(), r.state_style),
                    Span::raw("  "),
                    Span::raw(r.source).fg(Color::DarkGray),
                ]);
                if Some(pos) == cursor_pos {
                    line.bg(Color::Indexed(236))
                } else {
                    line
                }
            })
            .collect();
        if rows.is_empty() {
            let empty = if self.editor.filter().is_empty() {
                "no worktrees found (o to create one)"
            } else {
                "no worktrees match the filter"
            };
            frame.render_widget(Paragraph::new(empty).dark_gray(), list);
        } else {
            frame.render_widget(Paragraph::new(lines), list);
        }

        let status_line = self.status_line();
        let prompt_end = status_line.width();
        frame.render_widget(Paragraph::new(status_line), status);

        match self.editor.mode() {
            Mode::Command | Mode::Filter => {
                frame.set_cursor_position((status.x + to_u16(prompt_end), status.y));
            }
            Mode::Normal | Mode::Insert => {
                if let Some(pos) = cursor_pos {
                    let r = &rows[pos];
                    let before: String = r.name.chars().take(col).collect();
                    let x = width(r.mark) + 1 + repo_width + 2 + width(&before);
                    let y = pos - self.scroll;
                    frame.set_cursor_position((list.x + to_u16(x), list.y + to_u16(y)));
                }
            }
        }
        self.render_summary(frame, status);
    }

    /// Mode tag followed by the prompt, filter or message. The cursor sits at its end in
    /// command and filter mode.
    fn status_line(&self) -> Line<'_> {
        let (tag, color) = match self.editor.mode() {
            Mode::Normal => ("NORMAL", Color::Blue),
            Mode::Insert => ("INSERT", Color::Green),
            Mode::Command => ("COMMAND", Color::Yellow),
            Mode::Filter => ("FILTER", Color::Magenta),
        };
        let mut spans = vec![mode_tag(tag, color), Span::raw(" ")];
        match self.editor.mode() {
            Mode::Command => spans.push(Span::raw(format!(":{}", self.editor.prompt()))),
            Mode::Filter => spans.push(Span::raw(format!("/{}", self.editor.filter()))),
            Mode::Normal | Mode::Insert => {
                if let Some(message) = self.editor.message() {
                    spans.push(Span::raw(message));
                } else if !self.editor.filter().is_empty() {
                    spans.push(Span::raw(format!("/{}", self.editor.filter())).dark_gray());
                }
            }
        }
        Line::from(spans)
    }

    fn render_summary(&self, frame: &mut Frame, status: Rect) {
        let mut parts = Vec::new();
        if self.loading > 0 {
            parts.push(format!("loading {}", self.loading));
        }
        parts.push(format!("{} worktrees", self.worktrees.len()));
        if self.editor.is_modified() {
            parts.push("[+]".into());
        }
        let summary = Line::from(format!("{} ", parts.join("  ")))
            .dark_gray()
            .right_aligned();
        frame.render_widget(summary, status);
    }
}

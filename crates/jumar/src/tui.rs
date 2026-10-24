use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use anyhow::Result;
use jumar_core::editor::{Action, Editor, Entry, Key, Mode};
use jumar_core::inspect::inspect_all;
use jumar_core::model::{Health, Worktree};
use jumar_core::plan::{self, Line as BufferLine};
use ratatui::crossterm::cursor::SetCursorStyle;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{DefaultTerminal, Frame};

use crate::discover;
use crate::format::{self, InspectResult};

pub enum Outcome {
    Quit,
    Open(PathBuf),
}

pub fn run() -> Result<Outcome> {
    let worktrees: Arc<[Worktree]> = discover::worktrees()?.into();
    let mut app = App::new(worktrees);
    let mut terminal = ratatui::try_init()?;
    let outcome = app.run(&mut terminal);
    ratatui::restore();
    execute!(io::stdout(), SetCursorStyle::DefaultUserShape)?;
    outcome
}

struct App {
    worktrees: Arc<[Worktree]>,
    results: Vec<Option<InspectResult>>,
    loading: usize,
    inspections: Receiver<(usize, InspectResult)>,
    editor: Editor,
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

impl App {
    fn new(worktrees: Arc<[Worktree]>) -> Self {
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
        Self {
            results: std::iter::repeat_with(|| None)
                .take(worktrees.len())
                .collect(),
            loading: worktrees.len(),
            inspections: inspect_all(&worktrees, discover::WORKERS),
            editor: Editor::new(entries),
            worktrees,
            scroll: 0,
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<Outcome> {
        let mut mode = None;
        loop {
            self.receive();
            if mode != Some(self.editor.mode()) {
                mode = Some(self.editor.mode());
                let shape = match self.editor.mode() {
                    Mode::Normal => SetCursorStyle::SteadyBlock,
                    Mode::Insert | Mode::Command | Mode::Filter => SetCursorStyle::SteadyBar,
                };
                execute!(io::stdout(), shape)?;
            }
            terminal.draw(|frame| self.render(frame))?;

            let timeout = if self.loading > 0 {
                Duration::from_millis(30)
            } else {
                Duration::from_secs(60)
            };
            if !event::poll(timeout)? {
                continue;
            }
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && let Some(key) = convert(key)
                && let Some(action) = self.editor.handle(key)
                && let Some(outcome) = self.act(action)
            {
                return Ok(outcome);
            }
        }
    }

    fn receive(&mut self) {
        for (index, result) in self.inspections.try_iter() {
            self.results[index] = Some(result);
            self.loading -= 1;
        }
    }

    fn act(&mut self, action: Action) -> Option<Outcome> {
        match action {
            Action::Quit => return Some(Outcome::Quit),
            Action::Open(_) if self.editor.is_modified() => {
                self.editor
                    .set_message("unsaved changes (:w to apply, :q! to discard)");
            }
            Action::Open(id) => return Some(Outcome::Open(self.worktrees[id.0].path.clone())),
            Action::Write { quit } => match plan::plan(&self.worktrees, self.editor.lines()) {
                Ok(operations) if operations.is_empty() => {
                    if quit {
                        return Some(Outcome::Quit);
                    }
                    self.editor.set_message("no changes");
                }
                Ok(operations) => self.editor.set_message(format!(
                    "{} change(s) planned; applying is not implemented yet",
                    operations.len()
                )),
                Err(errors) => {
                    let more = match errors.len() {
                        1 => String::new(),
                        n => format!(" (+{} more)", n - 1),
                    };
                    self.editor.set_message(format!("{}{more}", errors[0]));
                }
            },
            Action::Command(command) => {
                self.editor
                    .set_message(format!("not an editor command: {command}"));
            }
        }
        None
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

    fn render(&mut self, frame: &mut Frame) {
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
        let mut spans = vec![
            Span::raw(format!(" {tag} "))
                .fg(Color::Black)
                .bg(color)
                .bold(),
            Span::raw(" "),
        ];
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

fn convert(event: KeyEvent) -> Option<Key> {
    Some(match event.code {
        KeyCode::Char(c) if event.modifiers.contains(KeyModifiers::CONTROL) => Key::Ctrl(c),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Esc => Key::Esc,
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        _ => return None,
    })
}

fn width(text: &str) -> usize {
    Span::raw(text).width()
}

fn column_width<'a>(cells: impl Iterator<Item = &'a str>) -> usize {
    cells.map(width).max().unwrap_or(0)
}

fn pad(text: &str, to: usize) -> String {
    format!("{text}{}", " ".repeat(to.saturating_sub(width(text))))
}

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_should_use_display_width() {
        assert_eq!(pad("🟢", 3), "🟢 ");
    }

    #[test]
    fn convert_should_map_control_chars() {
        let event = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);

        assert_eq!(convert(event), Some(Key::Ctrl('r')));
    }
}
